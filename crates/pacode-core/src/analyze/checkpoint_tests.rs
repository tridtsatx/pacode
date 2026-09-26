use super::*;

const SEED: u64 = 0x1234_5678_9ABC_DEF0;

fn checkpoints() -> Checkpoints {
    Checkpoints::new(SEED)
}

#[test]
fn nothing_is_asked_before_the_interval() {
    let mut c = checkpoints();
    for _ in 0..4 {
        assert_eq!(c.due(5, 0), None, "asked early");
    }
    assert!(c.due(5, 0).is_some(), "asked on the fifth action");
}

#[test]
fn a_zero_interval_never_asks() {
    let mut c = checkpoints();
    for _ in 0..1_000 {
        assert_eq!(c.due(0, 0), None);
    }
    assert_eq!(c.report().checkpoints, 0);
}

#[test]
fn the_right_word_scores_and_the_wrong_one_costs_a_point() {
    let mut c = checkpoints();
    let word = c.due(1, 0).expect("due");
    c.settle(Some(&word), 0);
    assert_eq!(c.report().passed, 1);
    assert_eq!(c.report().score, 1.0, "a correct word is one point");

    c.due(1, 0).expect("due");
    c.settle(Some("not-the-word"), 0);
    assert_eq!(c.report().wrong, 1);
    assert_eq!(c.report().score, 0.0, "a wrong word takes the point back");
}

#[test]
fn saying_nothing_costs_twice_as_much_as_saying_the_wrong_thing() {
    // The point of the weighting: a model that ignored the instruction is
    // treated as worse than one that tried and got it wrong.
    let mut ignored = checkpoints();
    let mut wrong = checkpoints();
    ignored.due(1, 0);
    ignored.settle(None, 0);
    let word = wrong.due(1, 0).expect("due");
    wrong.settle(Some(&format!("{word}x")), 0);
    assert_eq!(ignored.report().score, -2.0);
    assert_eq!(wrong.report().score, -1.0);
}

#[test]
fn case_and_padding_do_not_decide_the_score() {
    let mut c = checkpoints();
    let word = c.due(1, 0).expect("due");
    c.settle(Some(&format!("  {}  ", word.to_uppercase())), 0);
    assert_eq!(c.report().passed, 1, "a word is a word");
}

#[test]
fn a_word_lost_to_compaction_is_not_the_models_fault() {
    let mut c = checkpoints();
    c.due(1, 7);
    // Compaction ran between the question and the answer.
    c.settle(None, 8);
    let report = c.report();
    assert_eq!(report.lost_to_compaction, 1);
    assert_eq!(report.ignored, 0, "not scored as ignored");
    assert_eq!(report.score, 0.0, "and it costs nothing");
    assert_eq!(report.scored_checkpoints(), 0, "it leaves the denominator");
}

#[test]
fn a_word_issued_after_compaction_is_still_scored() {
    let mut c = checkpoints();
    let word = c.due(1, 8);
    c.settle(word.as_deref(), 8);
    assert_eq!(
        c.report().passed,
        1,
        "the generation it was born in is the one that counts"
    );
}

#[test]
fn an_unanswered_checkpoint_is_not_replaced_by_the_next_one() {
    // Otherwise the model could be asked a second question and whichever
    // answer arrived would be scored against the first word.
    let mut c = checkpoints();
    assert!(c.due(1, 0).is_some());
    for _ in 0..50 {
        assert_eq!(c.due(1, 0), None, "a second question while one is open");
    }
    assert_eq!(c.report().checkpoints, 0, "and nothing is scored for it");
}

#[test]
fn the_schedule_repeats_old_words_and_issues_new_ones() {
    let mut c = checkpoints();
    let mut issued = Vec::new();
    for _ in 0..30 {
        if let Some(word) = c.due(1, 0) {
            c.settle(Some(&word), 0);
            issued.push(word);
        }
    }
    assert_eq!(issued.len(), 30);
    let fresh: Vec<&String> = issued
        .iter()
        .filter(|word| issued.iter().take_while(|other| *other != *word).count() == 0)
        .collect();
    assert!(
        fresh.len() >= 30 / NEW_WORD_EVERY as usize,
        "expected roughly one fresh word in {NEW_WORD_EVERY}, got {} of {}",
        fresh.len(),
        issued.len()
    );
    assert!(
        issued
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            < issued.len(),
        "most checkpoints must reuse a word, or nothing is being measured"
    );
}

#[test]
fn the_same_seed_replays_the_same_run() {
    let run = |seed: u64| {
        let mut c = Checkpoints::new(seed);
        (0..12)
            .filter_map(|_| {
                let word = c.due(1, 0)?;
                c.settle(Some(&word), 0);
                Some(word)
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        run(SEED),
        run(SEED),
        "a scorecard you cannot replay is not a measurement"
    );
    assert_ne!(
        run(SEED),
        run(SEED ^ 0xFF),
        "and a different seed must give a different run"
    );
}

#[test]
fn a_word_is_never_issued_twice_as_a_fresh_one() {
    let mut c = checkpoints();
    let mut fresh = Vec::new();
    for _ in 0..26 {
        let Some(word) = c.due(1, 0) else { continue };
        let was_new = !fresh.contains(&word);
        if was_new {
            fresh.push(word.clone());
        }
        c.settle(Some(&word), 0);
    }
    // Every word the pool could produce appears at most once as a *fresh* draw.
    let mut sorted = fresh.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), fresh.len(), "a fresh word repeated");
}

#[test]
fn settling_twice_scores_once() {
    // A retried turn must not be able to bank two answers for one question.
    let mut c = checkpoints();
    let word = c.due(1, 0).expect("due");
    c.settle(Some(&word), 0);
    c.settle(Some(&word), 0);
    assert_eq!(c.report().passed, 1);
    assert_eq!(c.report().checkpoints, 1);
}

#[test]
fn the_outstanding_word_is_readable_while_it_is_open() {
    let mut c = checkpoints();
    assert_eq!(c.pending_word(), None);
    let word = c.due(1, 0).expect("due");
    assert_eq!(c.pending_word(), Some(word.as_str()));
    c.settle(Some(&word), 0);
    assert_eq!(c.pending_word(), None, "settled words stop being pending");
}
