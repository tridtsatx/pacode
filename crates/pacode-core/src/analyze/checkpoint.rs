//! The context-quality checkpoint: a code word the model has to remember.
//!
//! Every `analyze_checkpoint_every` actions the turn loop hands the model an
//! instruction to call a tool with a word it was never told the meaning of,
//! and the word is the question. A model that answers it is holding on to its
//! context; one that says something else is not. The word is not described as a
//! code word, because a model told it is a test answers the test instead of the
//! question.
//!
//! # Why a word and not a question
//!
//! Asking "what was word 3?" measures the same thing and invites the model to
//! pattern-match the transcript instead of reading it. A word with no meaning
//! can only come from the context, and the *distance* is the point: words
//! issued early are asked back late, so a long context dilutes them.
//!
//! # The schedule
//!
//! Mostly a word the model has already seen, so a checkpoint asks whether it
//! kept something; every third checkpoint issues a new word, so the model cannot
//! pass by pattern and the run also measures holding on to *new* material. The
//! draw is seeded, so a run replays identically — the schedule is part of what
//! the page reports, and an unreproducible scorecard is not a measurement.
//!
//! # Compaction
//!
//! A checkpoint lives in the transcript as an ordinary user message, so
//! compaction can take it away before it is asked. That is a fact about the
//! session, not about the model's memory: it is counted separately and left out
//! of the score's denominator.
//!
//! # The clock
//!
//! Nothing here owns one. The turn loop asks [`Checkpoints::due`] at the top of
//! a turn and settles the answer at the bottom, so an idle client arms no timer
//! and a checkpoint costs nothing until a turn is already running.

use pacode_types::analyze::{CheckpointOutcome, ContextQualityReport};

/// How many checkpoints before a new word is issued. One in three: enough that
/// the model cannot rely on the word being an old one, few enough that most
/// checkpoints still ask about something it has had time to lose.
pub const NEW_WORD_EVERY: u32 = 3;

/// Word pool. Short, unambiguous, typeable — nothing a model would produce by
/// accident when it is guessing.
const WORDS: &[&str] = &[
    "amber",
    "basalt",
    "cinder",
    "dune",
    "ember",
    "flint",
    "gossamer",
    "harbour",
    "indigo",
    "jetty",
    "kelvin",
    "lumen",
    "marrow",
    "nimbus",
    "ochre",
    "pyrite",
    "quartz",
    "rill",
    "slate",
    "tundra",
    "umber",
    "verdigris",
    "willow",
    "xenon",
    "yarrow",
    "zephyr",
];

/// The instruction the model sees. It names the word and the tool and nothing
/// else: it does not say the word is a test, because a model told it is a test
/// answers the test rather than reaching back into the context, and that is
/// the whole measurement.
pub fn instruction(word: &str) -> String {
    format!(
        "Context note: earlier in this conversation you were given the word \
         `{word}`. Call the `checkpoint` tool now with that word, then carry on \
         with what you were doing."
    )
}

/// One issued checkpoint: the word, and the compaction generation it was issued
/// under. A different generation at settle time means the context it lived in
/// is gone.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Pending {
    word: String,
    generation: u64,
}

/// The session's checkpoint state: the counter, the words already issued, the
/// one outstanding, and the scorecard they feed.
#[derive(Debug)]
pub struct Checkpoints {
    /// Actions taken since the last checkpoint.
    actions: u32,
    /// Every word issued so far, oldest first. The sample is drawn from here,
    /// which is what makes a checkpoint a memory question.
    issued: Vec<String>,
    /// Seed for the draw.
    seed: u64,
    issued_count: u32,
    pending: Option<Pending>,
    report: ContextQualityReport,
}

impl Default for Checkpoints {
    fn default() -> Self {
        Self::new(0x9E37_79B9_7F4A_7C15)
    }
}

impl Checkpoints {
    /// `seed` fixes the word sequence, so a run replays identically.
    pub fn new(seed: u64) -> Self {
        Self {
            actions: 0,
            issued: Vec::new(),
            seed,
            issued_count: 0,
            pending: None,
            report: ContextQualityReport::default(),
        }
    }

    pub fn report(&self) -> ContextQualityReport {
        self.report
    }

    /// The outstanding word, if a checkpoint has been asked and not yet
    /// answered.
    pub fn pending_word(&self) -> Option<&str> {
        self.pending.as_ref().map(|pending| pending.word.as_str())
    }

    /// Count one action; return the word to ask for when this is the one due.
    ///
    /// An outstanding checkpoint is never replaced: asking a second question
    /// while the first is unanswered would score whichever answer arrived, and
    /// a word that can be answered by the wrong question is not a measurement.
    pub fn due(&mut self, every: u32, generation: u64) -> Option<String> {
        if every == 0 || self.pending.is_some() {
            return None;
        }
        self.actions = self.actions.saturating_add(1);
        if self.actions < every {
            return None;
        }
        self.actions = 0;
        let word = self.next_word();
        self.issued.push(word.clone());
        self.pending = Some(Pending {
            word: word.clone(),
            generation,
        });
        Some(word)
    }

    /// Settle the outstanding checkpoint against what the model did.
    ///
    /// `answer` is the word the `checkpoint` tool was called with, if it was
    /// called at all. A word whose context was compacted away is recorded as
    /// lost and never scored, whatever the model said afterwards — it could not
    /// have known the word was ever there.
    pub fn settle(&mut self, answer: Option<&str>, generation: u64) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let outcome = if pending.generation != generation {
            CheckpointOutcome::LostToCompaction
        } else {
            match answer {
                None => CheckpointOutcome::Ignored,
                Some(word) if word.trim().eq_ignore_ascii_case(&pending.word) => {
                    CheckpointOutcome::Passed
                }
                Some(_) => CheckpointOutcome::Wrong,
            }
        };
        self.report.record(outcome);
    }

    /// The next word: a new one every [`NEW_WORD_EVERY`]-th checkpoint, and
    /// otherwise a uniform draw from the words already issued.
    fn next_word(&mut self) -> String {
        self.issued_count = self.issued_count.saturating_add(1);
        // The first two checkpoints are always new: a draw needs more than one
        // candidate, and the model should never be asked for a word it has seen
        // exactly once and can therefore be guessing at.
        if self.issued.is_empty()
            || self.issued_count.is_multiple_of(NEW_WORD_EVERY)
            || self.issued.len() < 2
        {
            return self.fresh_word();
        }
        let pick = (self.next_rand() % self.issued.len() as u64) as usize;
        self.issued[pick].clone()
    }

    fn fresh_word(&mut self) -> String {
        // Walk the draw until the word is one this session has not used: a
        // repeat would let a model that saw the word once pass a "new" word.
        for _ in 0..WORDS.len() {
            let candidate = WORDS[(self.next_rand() % WORDS.len() as u64) as usize].to_string();
            if !self.issued.contains(&candidate) {
                return candidate;
            }
        }
        // The pool outlasts any sensible session; if it were exhausted, a used
        // word is still better than a panic inside the turn loop.
        WORDS[self.issued.len() % WORDS.len()].to_string()
    }

    /// xorshift64: one number per call, no dependency, and the same sequence on
    /// every platform — which matters because the schedule is part of the
    /// reported scorecard.
    fn next_rand(&mut self) -> u64 {
        let mut x = self.seed;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.seed = x;
        x
    }
}

#[cfg(test)]
#[path = "checkpoint_tests.rs"]
mod checkpoint_tests;
