use super::*;

#[test]
fn journey_steps_render_in_the_requested_language() {
    let board = TransitStep::Board {
        line: "Island",
        stop: "Central",
        headsign: Some("Chai Wan"),
    };
    for (language, expected) in [
        (None, "Board Island toward Chai Wan at Central."),
        (Some("zh-HK"), "在Central登上往Chai Wan的Island。"),
        (
            Some("de"),
            "An Central in Island Richtung Chai Wan einsteigen.",
        ),
    ] {
        let (phrasebook, warning) = Phrasebook::for_language(language);
        assert_eq!(warning, None);
        assert_eq!(phrasebook.transit_step(board), expected);
    }
    let (phrasebook, warning) = Phrasebook::for_language(Some("xx"));
    assert_eq!(phrasebook.language(), "en");
    assert!(warning.is_some());
    assert_eq!(
        phrasebook.transit_step(TransitStep::Street {
            mode: StreetLegMode::Bicycle,
            from: "Home",
            to: "Pier"
        }),
        "Cycle from Home to Pier."
    );
}
