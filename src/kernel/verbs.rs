//! The verbs: admit, classify, label, forget, pull, send, ask, why, embed, answer.
//! Every one runs through a `Kernel`, which is a config plus the ports.

use anyhow::{Context, Result};
use std::cell::RefCell;

use super::classify;
use super::inbox::{parse_duration, parse_when, Config, Inbox, Question};
use super::item::{Draft, Item, NewItem};
use super::ports::{Adapters, StaleHint, Store};

const ANSWER_ITEMS: usize = 12;
const ANSWER_CLIP: usize = 1500;
const ANSWER_SYSTEM: &str =
    "You answer a question from someone's inbox using only the items given. \
Cite every item you rely on as #id. If the items do not answer the question, say so plainly.";

/// What `answer` returns: the model's text, the items it cited, and every
/// item it was shown.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Answer {
    pub text: String,
    pub cites: Vec<i64>,
    pub considered: Vec<i64>,
}

pub struct Kernel<'a> {
    pub config: &'a Config,
    pub store: &'a dyn Store,
    adapters: &'a dyn Adapters,
    warnings: RefCell<Vec<String>>,
}

impl<'a> Kernel<'a> {
    pub fn new(config: &'a Config, store: &'a dyn Store, adapters: &'a dyn Adapters) -> Self {
        Self {
            config,
            store,
            adapters,
            warnings: RefCell::new(Vec::new()),
        }
    }

    /// Things that went wrong but did not stop a verb, such as a classifier
    /// that could not decide. Drained on read.
    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut self.warnings.borrow_mut())
    }

    fn warn(&self, msg: String) {
        self.warnings.borrow_mut().push(msg);
    }

    /// Upsert, classify from the root down, then embed if the host has an embedder.
    pub fn admit(&self, item: NewItem) -> Result<i64> {
        let (id, _) = self.store.upsert(&item)?;
        self.classify(id)?;
        if let Err(e) = self.embed(id) {
            self.warn(format!("embed #{id}: {e:#}"));
        }
        Ok(id)
    }

    /// Store the item's vector. Nothing happens without an embedder in the
    /// config. Returns whether a vector was written.
    pub fn embed(&self, id: i64) -> Result<bool> {
        let Some(spec) = &self.config.embedder else {
            return Ok(false);
        };
        if !self.store.unembedded()?.contains(&id) {
            return Ok(false);
        }
        let item = self.store.get(id)?;
        let vector = self.adapters.embedder(spec)?.embed(&item.text())?;
        self.store.set_vector(id, &vector)?;
        Ok(true)
    }

    /// Embed every item that has no vector yet. Failures are warnings.
    pub fn embed_missing(&self) -> Result<usize> {
        let mut n = 0;
        for id in self.store.unembedded()? {
            match self.embed(id) {
                Ok(true) => n += 1,
                Ok(false) => {}
                Err(e) => self.warn(format!("embed #{id}: {e:#}")),
            }
        }
        Ok(n)
    }

    /// A query's vector, in the same space as the items'.
    pub fn near(&self, text: &str) -> Result<Vec<f32>> {
        let spec = self
            .config
            .embedder
            .as_ref()
            .context("no embedder in config")?;
        self.adapters.embedder(spec)?.embed(text)
    }

    /// Ask the model a question over an inbox: retrieve by words and by
    /// meaning, hand the model the items, and keep the ids it cites.
    pub fn answer(&self, chain: &[&Inbox], question: &str) -> Result<Answer> {
        let spec = self.config.model.as_ref().context("no model in config")?;
        let model = self.adapters.model(spec)?;
        let mut considered: Vec<Item> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut take = |items: Vec<Item>| {
            for it in items {
                if seen.insert(it.id) && considered.len() < ANSWER_ITEMS {
                    considered.push(it);
                }
            }
        };
        let mut by_words = Question::of(chain);
        by_words.text = Some(question.to_string());
        by_words.limit = Some(ANSWER_ITEMS);
        take(self.store.ask(&by_words)?);
        if self.config.embedder.is_some() {
            let mut by_meaning = Question::of(chain);
            by_meaning.near = Some(self.near(question)?);
            by_meaning.limit = Some(ANSWER_ITEMS);
            take(self.store.ask(&by_meaning)?);
        }
        let mut user = format!("question: {question}\n\nitems:\n");
        for it in &considered {
            let from = it
                .from
                .as_ref()
                .map(|a| a.name.clone().unwrap_or_else(|| a.id.clone()))
                .unwrap_or_default();
            user.push_str(&format!(
                "\n### #{} {}\nfrom: {from}  when: {}  source: {}\n{}\n",
                it.id,
                it.title,
                it.when(),
                it.source_id,
                clip(&it.text(), ANSWER_CLIP)
            ));
        }
        let text = model.complete(ANSWER_SYSTEM, &user)?;
        let cites = cited_ids(&text)
            .into_iter()
            .filter(|id| seen.contains(id))
            .collect();
        Ok(Answer {
            text,
            cites,
            considered: considered.iter().map(|i| i.id).collect(),
        })
    }

    /// Enter the root, run its classifiers, then every child the item now
    /// matches, recursively. A label stamped on the way down can open a child.
    pub fn classify(&self, id: i64) -> Result<()> {
        let mut item = self.store.get(id)?;
        self.apply(&self.config.classifier, &mut item)?;
        for inbox in &self.config.inbox {
            self.enter(inbox, &mut item)?;
        }
        Ok(())
    }

    fn enter(&self, inbox: &Inbox, item: &mut Item) -> Result<()> {
        if !Question::of(&[inbox]).matches(item) {
            return Ok(());
        }
        self.apply(&inbox.classifier, item)?;
        for child in &inbox.inbox {
            self.enter(child, item)?;
        }
        Ok(())
    }

    fn apply(&self, specs: &[super::inbox::ClassifierSpec], item: &mut Item) -> Result<()> {
        for spec in specs {
            let classifier = match classify::build(spec)? {
                Some(c) => c,
                None => self.adapters.classifier(spec)?,
            };
            if classifier.once() && self.store.classified(item.id, spec.id.as_str())? {
                continue;
            }
            let label = match classifier.classify(item) {
                Ok(label) => {
                    if classifier.once() {
                        self.store.mark_classified(item.id, &spec.id)?;
                    }
                    label
                }
                Err(e) => {
                    // Not remembered, so the next pass tries again.
                    self.warn(format!("classifier {}: {e:#}", spec.id));
                    None
                }
            };
            if let Some(label) = label {
                if !item.labels.contains(&label) {
                    self.store.add_label(item.id, &label)?;
                    item.labels.push(label);
                }
            }
        }
        Ok(())
    }

    /// Add and remove labels, then classify so a newly matching child can fire.
    pub fn label(&self, id: i64, add: &[String], remove: &[String]) -> Result<()> {
        for l in add {
            self.store.add_label(id, l)?;
        }
        for l in remove {
            self.store.remove_label(id, l)?;
        }
        self.classify(id)
    }

    pub fn forget(&self, id: i64) -> Result<bool> {
        self.store.delete(id)
    }

    /// Pull every source. Returns how many items were new.
    pub fn pull(&self) -> Result<usize> {
        let mut new = 0;
        for spec in &self.config.source {
            let source = self.adapters.source(spec)?;
            for item in source.pull()? {
                let existed = self
                    .store
                    .find(&item.source_id, &item.foreign_id)?
                    .is_some();
                self.admit(item)?;
                new += usize::from(!existed);
            }
        }
        Ok(new)
    }

    /// Items answering an inbox chain, newest first (or by start when timed).
    pub fn ask(&self, chain: &[&Inbox]) -> Result<Vec<Item>> {
        self.store.ask(&Question::of(chain))
    }

    /// Drop stale items: a passed `end`, or an untimed item older than the
    /// source's (else the host's) `forget_after`. Kept labels never go.
    pub fn forget_stale(&self) -> Result<usize> {
        let keep = self.config.keep();
        let now = chrono::Utc::now();
        let mut n = 0;
        for hint in self.store.stale()? {
            if hint.labels.iter().any(|l| keep.contains(l)) {
                continue;
            }
            if self.is_stale(&hint, now) && self.store.delete(hint.id)? {
                n += 1;
            }
        }
        Ok(n)
    }

    fn is_stale(&self, hint: &StaleHint, now: chrono::DateTime<chrono::Utc>) -> bool {
        // `end` is a deadline; a start-only item is just a moment.
        if let Some(end) = nonempty(hint.end.as_deref()) {
            return parse_when(end).is_some_and(|dt| dt < now);
        }
        let after = self
            .config
            .source(&hint.source_id)
            .and_then(|s| s.forget_after.as_deref())
            .or(self.config.forget_after.as_deref())
            .and_then(parse_duration);
        match (after, parse_when(&hint.created_at)) {
            (Some(after), Some(created)) => now.signed_duration_since(created) > after,
            _ => false,
        }
    }

    /// Hand the draft to its source, then admit what came back. A reply
    /// joins the parent's thread, starting one if the parent had none.
    pub fn send(&self, draft: Draft) -> Result<i64> {
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
                self.store.set_thread(pid, Some(&thread))?;
            }
            draft.thread = Some(thread);
            reply_foreign = Some(parent.foreign_id);
        }
        if draft.title.trim().is_empty() {
            draft.title = "untitled".into();
        }
        let spec = if draft.source_id.is_empty() {
            self.config.source.first()
        } else {
            self.config.source(&draft.source_id)
        }
        .with_context(|| format!("no source `{}`", draft.source_id))?;
        let source = self.adapters.source(spec)?;
        let mut item = source.send(&draft, reply_foreign.as_deref())?;
        item.source_id = spec.id.clone();
        item.thread = draft.thread.clone();
        item.in_reply_to = reply_foreign;
        item.to = draft.to.clone();
        self.admit(item)
    }

    /// Which of the chain's labels the item carries, and which classifiers on
    /// the way down could have stamped them. A label can also come from a hand.
    pub fn why(&self, item: &Item, path: &[String]) -> String {
        let refs: Vec<&str> = path.iter().map(String::as_str).collect();
        let chain = self.config.chain(&refs).unwrap_or_default();
        let has = |l: &str| item.labels.iter().any(|x| x == l);
        let matched: Vec<&str> = chain
            .iter()
            .flat_map(|ib| ib.labels.iter())
            .map(String::as_str)
            .filter(|l| has(l))
            .collect();
        let fired: Vec<&str> = self
            .config
            .classifier
            .iter()
            .chain(chain.iter().flat_map(|ib| ib.classifier.iter()))
            .filter(|c| c.label.as_deref().is_some_and(has) || c.labels.iter().any(|l| has(l)))
            .map(|c| c.id.as_str())
            .collect();
        let dash = |v: Vec<&str>| {
            if v.is_empty() {
                "-".to_string()
            } else {
                v.join(" ")
            }
        };
        format!("labels: {}  classifiers: {}", dash(matched), dash(fired))
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

fn clip(s: &str, max: usize) -> &str {
    let mut end = max.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn nonempty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}
