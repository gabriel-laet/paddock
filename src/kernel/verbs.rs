//! The verbs: admit, classify, label, forget, pull, send, ask, why, embed, answer.
//! Every one runs on a `Kernel`: the config, a clock, and the resolved ports.
//! Verbs return what happened, warnings included; nothing is kept on the side.

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};

use super::inbox::{parse_duration, parse_when, rfc3339, ClassifierSpec, Config, Inbox, Question};
use super::item::{By, Draft, Item, Label, NewItem};
use super::ports::{Brief, Classifier, Embedder, Fact, Model, Source, StaleHint, Store};

const ANSWER_ITEMS: usize = 12;

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

/// An item is in. Warnings are things that did not stop it: a classifier
/// that could not decide, an embedder that was down.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Admitted {
    pub id: i64,
    pub warnings: Vec<String>,
}

/// A count, and what went wrong along the way.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Report {
    pub count: usize,
    pub warnings: Vec<String>,
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

impl Kernel<'_> {
    /// Upsert, classify from the root down, then embed if the host has an embedder.
    pub fn admit(&self, item: NewItem) -> Result<Admitted> {
        let (id, _) = self.store.upsert(&item)?;
        let mut warnings = self.classify(id)?;
        if let Err(e) = self.embed(id) {
            warnings.push(format!("embed #{id}: {e:#}"));
        }
        Ok(Admitted { id, warnings })
    }

    /// Enter the root, run its classifiers, then every child the item now
    /// matches, recursively. A label stamped on the way down can open a child.
    /// Returns warnings from classifiers that could not decide.
    pub fn classify(&self, id: i64) -> Result<Vec<String>> {
        let mut item = self.store.get(id)?;
        let mut warnings = Vec::new();
        self.apply(&self.config.classifier, &mut item, &mut warnings)?;
        for inbox in &self.config.inbox {
            self.enter(inbox, &mut item, &mut warnings)?;
        }
        Ok(warnings)
    }

    fn enter(&self, inbox: &Inbox, item: &mut Item, warnings: &mut Vec<String>) -> Result<()> {
        if !Question::of_at(&[inbox], self.now).matches(item) {
            return Ok(());
        }
        self.apply(&inbox.classifier, item, warnings)?;
        for child in &inbox.inbox {
            self.enter(child, item, warnings)?;
        }
        Ok(())
    }

    fn apply(
        &self,
        specs: &[ClassifierSpec],
        item: &mut Item,
        warnings: &mut Vec<String>,
    ) -> Result<()> {
        for spec in specs {
            let Some(classifier) = self.classifiers.get(&spec.id) else {
                warnings.push(format!("classifier {}: not resolved", spec.id));
                continue;
            };
            if classifier.once() && self.store.classified(item.id, &spec.id)? {
                continue;
            }
            let label = match classifier.classify(item) {
                Ok(label) => {
                    if classifier.once() {
                        self.store
                            .note(item.id, Fact::Classified(spec.id.clone()))?;
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
    pub fn label(&self, id: i64, add: &[String], remove: &[String]) -> Result<Vec<String>> {
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
    /// source's (else the host's) `forget_after`. Kept labels never go.
    pub fn forget_stale(&self) -> Result<usize> {
        let keep = self.config.keep();
        let mut n = 0;
        for hint in self.store.stale()? {
            if hint.labels.iter().any(|l| keep.contains(l)) {
                continue;
            }
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
        item.in_reply_to = reply_foreign;
        item.to = draft.to.clone();
        self.admit(item)
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
