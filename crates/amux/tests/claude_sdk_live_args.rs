#[path = "claude_sdk_live/args.rs"]
mod args;

#[test]
fn live_suite_argument_grammar_covers_usage_all_names_and_unknowns() {
    let known = ["sdk_driver"];
    assert!(args::select(&[], &known).unwrap().is_empty());
    assert_eq!(args::select(&["all".into()], &known).unwrap(), [0]);
    assert_eq!(args::select(&["sdk_driver".into()], &known).unwrap(), [0]);
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
    assert!(args::select(&["all".into(), "sdk_driver".into()], &known).is_err());
}
