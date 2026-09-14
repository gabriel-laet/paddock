//! The verbs: admit, classify, label, forget, pull, send, ask, why.
//! Every one runs through a `Kernel`, which is a config plus the ports.

use anyhow::{Context, Result};
use std::cell::RefCell;

use super::classify;
use super::inbox::{parse_duration, parse_when, Config, Inbox, Question};
use super::item::{Draft, Item, NewItem};
use super::ports::{Adapters, StaleHint, Store};

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

    /// Upsert, then classify from the root down.
    pub fn admit(&self, item: NewItem) -> Result<i64> {
        let (id, _) = self.store.upsert(&item)?;
        self.classify(id)?;
        Ok(id)
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
            .filter(|c| c.label.as_deref().is_some_and(has))
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

fn nonempty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}
