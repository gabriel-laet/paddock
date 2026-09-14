//! The verbs: admit, classify, label, forget, pull, send, ask, why, embed, answer.
//! Every one runs on a `Kernel`: the config, a clock, and the resolved ports.
//! Verbs return what happened, warnings included; nothing is kept on the side.

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};

use super::inbox::{parse_duration, parse_when, rfc3339, ClassifierSpec, Config, Inbox, Question};
use super::item::{By, Cite, CiteKind, Draft, Item, Label, NewItem, READ, SENT};
use super::ports::{Brief, Classifier, Embedder, Fact, Model, Source, StaleHint, Store};

const ANSWER_ITEMS: usize = 12;
const THREAD_LIMIT: usize = 500;

pub struct Kernel<'a> {
    pub config: &'a Config,
    pub store: &'a dyn Store,
    /// In config order. `pull` walks them; `send` picks one by id.
    pub sources: Vec<(String, Box<dyn Source>)>,
    /// By spec id. Every spec in the config has one.
    pub classifiers: HashMap<String, Box<dyn Classifier>>,
    pub embedder: Option<Box<dyn Embedder>>,
    pub model: Option<Box<dyn Model>>,
    /// The moment every verb runs at. Set by the host, so tests can pick it.
    pub now: chrono::DateTime<chrono::Utc>,
}

/// An inbox with `then = ["notify"]` raised its hand for an item, once.
/// The kernel says when; the host says how (a desktop notification, a
/// sound, a command). Carries the title so a notifier need not look it up.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Notice {
    pub id: i64,
    pub inbox: String,
    pub title: String,
    /// The item's labels as of the notice, so `code:483920` can be shown.
    pub labels: Vec<String>,
}

/// What a pass through the inboxes said: warnings (a classifier that could
/// not decide, an embedder that was down), and the notices inboxes raised.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Told {
    pub warnings: Vec<String>,
    pub notices: Vec<Notice>,
}

/// An item is in. Warnings are things that did not stop it.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Admitted {
    pub id: i64,
    pub warnings: Vec<String>,
    pub notices: Vec<Notice>,
}

/// A count, and what was said along the way.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Report {
    pub count: usize,
    pub warnings: Vec<String>,
    pub notices: Vec<Notice>,
}

/// Why an item is in an inbox: the chain's labels it carries, each with who
/// put it there, and the labels a hand has denied.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Why {
    pub matched: Vec<Label>,
    pub denied: Vec<Label>,
}

/// What `answer` returns: the model's text, the items it cited, and every
/// item it was shown.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Answer {
    pub text: String,
    pub cites: Vec<i64>,
    pub considered: Vec<i64>,
}

/// Which of an inbox's `then` effects a pass may run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effects {
    /// Every effect: a normal pass.
    All,
    /// Everything but `send:`: labels, read, and notices, nothing leaves.
    Local,
    /// None: what an effect itself produced is classified, but fires no
    /// effects of its own, so an effect cannot chase its own output.
    None,
}

impl Effects {
    fn allows(self, effect: &str) -> bool {
        match self {
            Effects::All => true,
            Effects::Local => !effect.starts_with("send:"),
            Effects::None => false,
        }
    }
}

impl Kernel<'_> {
    /// Upsert, take the source's word on read, classify from the root down,
    /// then embed if the host has an embedder.
    pub fn admit(&self, item: NewItem) -> Result<Admitted> {
        self.admit_with(item, Effects::All)
    }

    fn admit_with(&self, item: NewItem, effects: Effects) -> Result<Admitted> {
        let (id, _) = self.store.upsert(&item)?;
        if item.read == Some(true) {
            let current = self.store.get(id)?;
            if !current.has(READ) && !current.denies(READ) {
                self.store
                    .note(id, Fact::Label(self.stamp(READ, By::Source)))?;
            }
        }
        let mut told = self.classify_with(id, effects)?;
        if let Err(e) = self.embed(id) {
            told.warnings.push(format!("embed #{id}: {e:#}"));
        }
        Ok(Admitted {
            id,
            warnings: told.warnings,
            notices: told.notices,
        })
    }

    /// Enter the root, run its classifiers, then every child the item now
    /// matches, recursively. A label stamped on the way down can open a child.
    pub fn classify(&self, id: i64) -> Result<Told> {
        self.classify_with(id, Effects::All)
    }

    /// A rehearsal: classify with every effect but `send:`, so what would
    /// be labelled, read, and paged shows, and nothing leaves. What a
    /// replay runs on its copy of the store.
    pub fn rehearse(&self, id: i64) -> Result<Told> {
        self.classify_with(id, Effects::Local)
    }

    fn classify_with(&self, id: i64, effects: Effects) -> Result<Told> {
        let mut item = self.store.get(id)?;
        let mut told = Told::default();
        self.apply(&self.config.classifier, &mut item, &mut told)?;
        for inbox in &self.config.inbox {
            self.enter(inbox, &inbox.name, effects, &mut item, &mut told)?;
        }
        Ok(told)
    }

    fn enter(
        &self,
        inbox: &Inbox,
        path: &str,
        effects: Effects,
        item: &mut Item,
        told: &mut Told,
    ) -> Result<()> {
        if !Question::of_at(&[inbox], self.now).matches(item) {
            return Ok(());
        }
        self.apply(&inbox.classifier, item, told)?;
        for effect in inbox.then.iter().filter(|e| effects.allows(e)) {
            // Once per entry. A failed effect is not remembered, so it retries.
            let key = format!("then:{path}:{effect}");
            if self.store.seen(item.id, &key)? {
                continue;
            }
            if self.effect(effect, path, item, told)? {
                self.store.note(item.id, Fact::Seen(key))?;
            }
        }
        for child in &inbox.inbox {
            let path = format!("{path}/{}", child.name);
            self.enter(child, &path, effects, item, told)?;
        }
        Ok(())
    }

    /// What an inbox does to an item that enters it. Returns whether it is
    /// done: a label already there counts, a send that failed does not.
    fn effect(&self, effect: &str, path: &str, item: &mut Item, told: &mut Told) -> Result<bool> {
        let by = By::Inbox(path.to_string());
        let put = |name: &str, item: &mut Item| -> Result<()> {
            if item.has(name) || item.denies(name) {
                return Ok(());
            }
            let label = self.stamp(name, by.clone());
            self.store.note(item.id, Fact::Label(label.clone()))?;
            item.labels.push(label);
            Ok(())
        };
        match effect.split_once(':').unwrap_or((effect, "")) {
            ("read", "") => put(READ, item).map(|_| true),
            ("label", name) if !name.is_empty() => put(name, item).map(|_| true),
            ("notify", "") => {
                told.notices.push(Notice {
                    id: item.id,
                    inbox: path.to_string(),
                    title: item.title.clone(),
                    labels: item.label_names(),
                });
                Ok(true)
            }
            ("send", source) if !source.is_empty() => {
                if item.has(SENT) {
                    return Ok(true);
                }
                let draft = Draft {
                    source_id: source.to_string(),
                    title: item.title.clone(),
                    body: item.text_body(),
                    thread: item.thread.clone(),
                    reply_to: item.reply_to(),
                    to: item.to.clone(),
                    ..Default::default()
                };
                match self.send_with(draft, Effects::None) {
                    Ok(sent) => {
                        told.warnings.extend(sent.warnings);
                        told.notices.extend(sent.notices);
                        put(SENT, item).map(|_| true)
                    }
                    Err(e) => {
                        told.warnings
                            .push(format!("{path}: send:{source} #{}: {e:#}", item.id));
                        Ok(false)
                    }
                }
            }
            _ => {
                told.warnings
                    .push(format!("{path}: unknown effect `{effect}`"));
                Ok(false)
            }
        }
    }

    fn stamp(&self, name: &str, by: By) -> Label {
        Label {
            name: name.to_string(),
            by,
            at: rfc3339(self.now),
        }
    }

    /// A hand marks an item read or unread. Unread is remembered: a source
    /// saying "read" later does not override it.
    pub fn read(&self, id: i64, read: bool) -> Result<Told> {
        let name = READ.to_string();
        if read {
            self.label(id, &[name], &[])
        } else {
            self.label(id, &[], &[name])
        }
    }

    /// The source a draft goes to from inside an inbox chain: the first source
    /// of the deepest inbox that names any. None means the host's first source.
    pub fn source_for(&self, chain: &[&Inbox]) -> Option<String> {
        chain
            .iter()
            .rev()
            .find_map(|ib| ib.sources.first().cloned())
    }

    fn apply(&self, specs: &[ClassifierSpec], item: &mut Item, told: &mut Told) -> Result<()> {
        let warnings = &mut told.warnings;
        for spec in specs {
            let Some(classifier) = self.classifiers.get(&spec.id) else {
                warnings.push(format!("classifier {}: not resolved", spec.id));
                continue;
            };
            if classifier.once() && self.store.seen(item.id, &spec.id)? {
                continue;
            }
            let label = match classifier.classify(item) {
                Ok(label) => {
                    if classifier.once() {
                        self.store.note(item.id, Fact::Seen(spec.id.clone()))?;
                    }
                    label
                }
                Err(e) => {
                    // Not remembered, so the next pass tries again.
                    warnings.push(format!("classifier {}: {e:#}", spec.id));
                    None
                }
            };
            if let Some(name) = label {
                // A hand's removal stands; a classifier does not argue with it.
                if item.has(&name) || item.denies(&name) {
                    continue;
                }
                let label = Label {
                    name,
                    by: By::Classifier(spec.id.clone()),
                    at: rfc3339(self.now),
                };
                self.store.note(item.id, Fact::Label(label.clone()))?;
                item.labels.push(label);
            }
        }
        Ok(())
    }

    /// A hand adds and removes labels, then classify runs so a newly matching
    /// child can fire. A removal is remembered: classifiers will not undo it.
    pub fn label(&self, id: i64, add: &[String], remove: &[String]) -> Result<Told> {
        let by_hand = |name: &String| Label {
            name: name.clone(),
            by: By::Hand,
            at: rfc3339(self.now),
        };
        for l in add {
            self.store.note(id, Fact::Label(by_hand(l)))?;
        }
        for l in remove {
            self.store.note(id, Fact::Unlabel(by_hand(l)))?;
        }
        self.classify(id)
    }

    pub fn forget(&self, id: i64) -> Result<bool> {
        self.store.delete(id)
    }

    /// Pull every source. Counts the items that were new.
    pub fn pull(&self) -> Result<Report> {
        let mut report = Report::default();
        for (_, source) in &self.sources {
            for item in source.pull()? {
                let existed = self
                    .store
                    .find(&item.source_id, &item.foreign_id)?
                    .is_some();
                let admitted = self.admit(item)?;
                report.count += usize::from(!existed);
                report.warnings.extend(admitted.warnings);
                report.notices.extend(admitted.notices);
            }
        }
        Ok(report)
    }

    /// Items answering an inbox chain, newest first (or by start when timed).
    pub fn ask(&self, chain: &[&Inbox]) -> Result<Vec<Item>> {
        self.store.ask(&self.question(chain))
    }

    /// The chain's question, as of this kernel's `now`.
    pub fn question(&self, chain: &[&Inbox]) -> Question {
        Question::of_at(chain, self.now)
    }

    /// Drop stale items: a passed `end`, or an untimed item older than the
    /// source's (else the host's) `forget_after`. The stale question is
    /// "everything without a kept label"; `keep` is just its `without`.
    pub fn forget_stale(&self) -> Result<usize> {
        let candidates = Question {
            without: self.config.keep(),
            ..Question::default()
        };
        let mut n = 0;
        for hint in self.store.stale(&candidates)? {
            if self.is_stale(&hint) && self.store.delete(hint.id)? {
                n += 1;
            }
        }
        Ok(n)
    }

    fn is_stale(&self, hint: &StaleHint) -> bool {
        // `end` is a deadline; a start-only item is just a moment.
        if let Some(end) = nonempty(hint.end.as_deref()) {
            return parse_when(end).is_some_and(|dt| dt < self.now);
        }
        let after = self
            .config
            .source(&hint.source_id)
            .and_then(|s| s.forget_after.as_deref())
            .or(self.config.forget_after.as_deref())
            .and_then(parse_duration);
        match (after, parse_when(&hint.created_at)) {
            (Some(after), Some(created)) => self.now.signed_duration_since(created) > after,
            _ => false,
        }
    }

    /// Hand the draft to its source, then admit what came back. A reply
    /// joins the parent's thread, starting one if the parent had none.
    pub fn send(&self, draft: Draft) -> Result<Admitted> {
        self.send_with(draft, Effects::All)
    }

    fn send_with(&self, draft: Draft, effects: Effects) -> Result<Admitted> {
        let mut draft = draft;
        let mut reply_foreign = None;
        if let Some(pid) = draft.reply_to {
            let parent = self.store.get(pid)?;
            if draft.source_id.is_empty() {
                draft.source_id = parent.source_id.clone();
            }
            if draft.title.trim().is_empty() {
                draft.title = reply_title(&parent);
            }
            let thread = draft
                .thread
                .clone()
                .or(parent.thread.clone())
                .unwrap_or_else(|| format!("{}:{}", parent.source_id, parent.foreign_id));
            if parent.thread.is_none() {
                self.store.note(pid, Fact::Thread(Some(thread.clone())))?;
            }
            draft.thread = Some(thread);
            reply_foreign = Some(parent.foreign_id);
        }
        if draft.title.trim().is_empty() {
            draft.title = "untitled".into();
        }
        let (id, source) = if draft.source_id.is_empty() {
            self.sources.first()
        } else {
            self.sources.iter().find(|(id, _)| *id == draft.source_id)
        }
        .with_context(|| format!("no source `{}`", draft.source_id))?;
        let mut item = source.send(&draft, reply_foreign.as_deref())?;
        item.source_id = id.clone();
        item.thread = draft.thread.clone();
        if let Some(f) = &reply_foreign {
            item.cites.push(Cite::reply(f));
        }
        item.to = draft.to.clone();
        self.admit_with(item, effects)
    }

    /// Store the item's vector. Nothing happens without an embedder.
    /// Returns whether a vector was written.
    pub fn embed(&self, id: i64) -> Result<bool> {
        let Some(embedder) = &self.embedder else {
            return Ok(false);
        };
        if !self.store.unembedded()?.contains(&id) {
            return Ok(false);
        }
        let item = self.store.get(id)?;
        let vector = embedder.embed(&item.text())?;
        self.store.note(id, Fact::Vector(vector))?;
        Ok(true)
    }

    /// Embed every item that has no vector yet. Failures are warnings.
    pub fn embed_missing(&self) -> Result<Report> {
        let mut report = Report::default();
        for id in self.store.unembedded()? {
            match self.embed(id) {
                Ok(true) => report.count += 1,
                Ok(false) => {}
                Err(e) => report.warnings.push(format!("embed #{id}: {e:#}")),
            }
        }
        Ok(report)
    }

    /// A query's vector, in the same space as the items'.
    pub fn near(&self, text: &str) -> Result<Vec<f32>> {
        self.embedder
            .as_ref()
            .context("no embedder in config")?
            .embed(text)
    }

    /// Ask the model a question over an inbox: retrieve by words and by
    /// meaning, hand the model the items, and keep the ids it cites.
    pub fn answer(&self, chain: &[&Inbox], question: &str) -> Result<Answer> {
        let model = self.model.as_ref().context("no model in config")?;
        let mut items: Vec<Item> = Vec::new();
        let mut seen = HashSet::new();
        let mut take = |found: Vec<Item>| {
            for it in found {
                if seen.insert(it.id) && items.len() < ANSWER_ITEMS {
                    items.push(it);
                }
            }
        };
        let mut by_words = self.question(chain);
        by_words.text = Some(question.to_string());
        by_words.limit = Some(ANSWER_ITEMS);
        take(self.store.ask(&by_words)?);
        if self.embedder.is_some() {
            let mut by_meaning = self.question(chain);
            by_meaning.near = Some(self.near(question)?);
            by_meaning.limit = Some(ANSWER_ITEMS);
            take(self.store.ask(&by_meaning)?);
        }
        let considered: Vec<i64> = items.iter().map(|i| i.id).collect();
        let text = model.answer(&Brief {
            question: question.to_string(),
            items,
        })?;
        let cites = cited_ids(&text)
            .into_iter()
            .filter(|id| considered.contains(id))
            .collect();
        Ok(Answer {
            text,
            cites,
            considered,
        })
    }

    /// Everything in the item's thread: the source's thread key when it has
    /// one, else whatever is joined to it by reply and forward cites, in
    /// either direction. Newest first.
    pub fn thread(&self, id: i64) -> Result<Vec<Item>> {
        let item = self.store.get(id)?;
        if let Some(key) = item.thread.as_deref().filter(|k| !k.is_empty()) {
            return self.store.thread(key);
        }
        let joins = |c: &Cite| matches!(c.kind, CiteKind::Reply | CiteKind::Forward);
        let mut seen = std::collections::BTreeMap::new();
        let mut todo = vec![item];
        while let Some(it) = todo.pop() {
            if seen.contains_key(&it.id) || seen.len() >= THREAD_LIMIT {
                continue;
            }
            for c in it.cites.iter().filter(|c| joins(c)) {
                if let Some(parent) = c.id.filter(|p| !seen.contains_key(p)) {
                    todo.push(self.store.get(parent)?);
                }
            }
            for child in self.store.citing(it.id)? {
                if child.cites.iter().any(|c| joins(c) && c.id == Some(it.id)) {
                    todo.push(child);
                }
            }
            seen.insert(it.id, it);
        }
        let mut out: Vec<Item> = seen.into_values().collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        Ok(out)
    }

    /// The chain's labels the item carries, with who put each one there, and
    /// what a hand has denied.
    pub fn why(&self, item: &Item, path: &[String]) -> Why {
        let refs: Vec<&str> = path.iter().map(String::as_str).collect();
        let chain = self.config.chain(&refs).unwrap_or_default();
        let wanted: Vec<&String> = chain.iter().flat_map(|ib| ib.labels.iter()).collect();
        let matched = item
            .labels
            .iter()
            .filter(|l| wanted.contains(&&l.name))
            .cloned()
            .collect();
        Why {
            matched,
            denied: item.denied.clone(),
        }
    }
}

/// `re: title`, without stacking.
pub fn reply_title(parent: &Item) -> String {
    let t = parent.title.trim();
    if t.is_empty() {
        "re:".into()
    } else if t.to_ascii_lowercase().starts_with("re:") {
        t.to_string()
    } else {
        format!("re: {t}")
    }
}

/// Every `#123` in the text, in order, once each.
fn cited_ids(text: &str) -> Vec<i64> {
    let mut out = Vec::new();
    for m in regex::Regex::new(r"#(\d+)")
        .expect("valid")
        .captures_iter(text)
    {
        if let Ok(id) = m[1].parse::<i64>() {
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

fn nonempty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}
