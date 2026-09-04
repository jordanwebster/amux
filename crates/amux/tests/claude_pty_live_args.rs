#[path = "claude_pty_live/args.rs"]
mod args;

#[test]
fn live_suite_argument_grammar_covers_usage_all_names_and_unknowns() {
    let known = ["semantic_chat", "external_readonly"];
    assert!(args::select(&[], &known).unwrap().is_empty());
    assert_eq!(args::select(&["all".into()], &known).unwrap(), [0, 1]);
    assert_eq!(
        args::select(
            &["external_readonly".into(), "semantic_chat".into()],
            &known
        )
        .unwrap(),
        [1, 0]
    );
    // A name that is no scenario is a filter that matched nothing, as cargo
    // hands a workspace-wide filter to this binary too; known names beside
    // it still select.
    assert!(
        args::select(&["missing".into()], &known)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        args::select(&["missing".into(), known[0].into()], &known).unwrap(),
        [0]
    );
    assert!(args::select(&["all".into(), "semantic_chat".into()], &known).is_err());
}
