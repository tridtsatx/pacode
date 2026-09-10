use super::*;

fn opts(n: usize) -> Vec<QuestionOption> {
    (0..n)
        .map(|i| QuestionOption::new(format!("option {i}"), format!("what {i} means")))
        .collect()
}

fn build(options: Vec<QuestionOption>, multi: bool) -> Result<Question, QuestionError> {
    Question::new(
        QuestionId::new("qst_1"),
        QuestionOrigin::new(AgentId::main(), "main", CallId::new("call_1")),
        "Storage",
        "Where should the cache live?",
        options,
        multi,
        1_000,
    )
}

#[test]
fn a_well_formed_question_keeps_its_options_in_order() {
    let q = build(opts(3), false).expect("build");
    assert_eq!(q.options.len(), 3);
    assert_eq!(q.options[0].label, "option 0");
    assert_eq!(q.header, "Storage");
    assert!(!q.multi_select);
    assert_eq!(q.recommended_index(), None);
}

#[test]
fn the_recommended_option_is_found_and_only_one_is_allowed() {
    let mut options = opts(3);
    options[1] = options[1].clone().recommended();
    let q = build(options.clone(), false).expect("build");
    assert_eq!(q.recommended_index(), Some(1));

    options[2] = options[2].clone().recommended();
    assert_eq!(
        build(options, false),
        Err(QuestionError::ManyRecommended(2))
    );
}

#[test]
fn a_question_must_offer_a_real_choice() {
    assert_eq!(build(opts(1), false), Err(QuestionError::TooFewOptions(1)));
    assert_eq!(
        build(opts(MAX_OPTIONS + 1), false),
        Err(QuestionError::TooManyOptions(MAX_OPTIONS + 1))
    );

    let mut options = opts(2);
    options[1].label = "   ".to_string();
    assert_eq!(build(options, false), Err(QuestionError::EmptyLabel(1)));

    let empty = Question::new(
        QuestionId::new("qst_1"),
        QuestionOrigin::new(AgentId::main(), "main", CallId::new("call_1")),
        "Storage",
        "   ",
        opts(2),
        false,
        0,
    );
    assert_eq!(empty, Err(QuestionError::EmptyQuestion));
}

#[test]
fn every_field_is_capped() {
    let long = QuestionOption::new(
        "l".repeat(LABEL_MAX_CHARS + 50),
        "d".repeat(DESCRIPTION_MAX_CHARS + 50),
    );
    let q = Question::new(
        QuestionId::new("qst_1"),
        QuestionOrigin::new(AgentId::main(), "main", CallId::new("call_1")),
        "h".repeat(HEADER_MAX_CHARS + 10),
        "q".repeat(QUESTION_MAX_CHARS + 100),
        vec![long, QuestionOption::new("short", "")],
        false,
        0,
    )
    .expect("build");

    assert_eq!(q.header.chars().count(), HEADER_MAX_CHARS);
    assert_eq!(q.question.chars().count(), QUESTION_MAX_CHARS);
    assert_eq!(q.options[0].label.chars().count(), LABEL_MAX_CHARS);
    assert_eq!(
        q.options[0].description.chars().count(),
        DESCRIPTION_MAX_CHARS
    );
}

#[test]
fn an_answer_renders_as_the_labels_that_were_chosen() {
    let q = build(opts(3), true).expect("build");

    let one = QuestionAnswer::choice(1);
    assert_eq!(one.render(&q), "option 1");

    let many = QuestionAnswer {
        selected: vec![0, 2],
        ..QuestionAnswer::default()
    };
    assert_eq!(many.render(&q), "option 0\noption 2");

    let typed = QuestionAnswer::typed("something else entirely");
    assert_eq!(typed.render(&q), "something else entirely");

    let both = QuestionAnswer {
        selected: vec![0],
        free_text: Some("and also this".to_string()),
        cancelled: false,
    };
    assert_eq!(both.render(&q), "option 0\nand also this");
}

#[test]
fn a_cancelled_or_empty_answer_says_so_rather_than_looking_like_a_choice() {
    let q = build(opts(2), false).expect("build");
    assert!(QuestionAnswer::cancelled().render(&q).contains("dismissed"));
    assert!(
        QuestionAnswer::default()
            .render(&q)
            .contains("answered with nothing")
    );

    // An index that is not in the question is dropped rather than trusted.
    let stray = QuestionAnswer {
        selected: vec![99],
        ..QuestionAnswer::default()
    };
    assert!(stray.render(&q).contains("answered with nothing"));
}

#[test]
fn typed_text_is_capped_too() {
    let q = build(opts(2), false).expect("build");
    let typed = QuestionAnswer::typed("t".repeat(FREE_TEXT_MAX_CHARS + 500));
    assert_eq!(typed.render(&q).chars().count(), FREE_TEXT_MAX_CHARS);
}
