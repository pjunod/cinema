use super::directory_path_bounds;

#[test]
fn scan_identity_directory_bounds_follow_the_stored_separator() {
    assert_eq!(
        directory_path_bounds("/media/Show_Name%/ ").expect("posix bounds"),
        (
            "/media/Show_Name%/ /".to_owned(),
            "/media/Show_Name%/ 0".to_owned()
        )
    );
    assert_eq!(
        directory_path_bounds(r"C:\media\Show_Name%\").expect("windows bounds"),
        (
            r"C:\media\Show_Name%\".to_owned(),
            r"C:\media\Show_Name%]".to_owned()
        )
    );
    assert!(directory_path_bounds("").is_err());
    assert!(directory_path_bounds("relative").is_err());
}

#[test]
fn scan_identity_manual_copy_family_is_exact_and_bounded() {
    use super::{directory_matches_show_path, show_directory_identity, show_directory_path_bounds};
    let base = "/media/City Watch";
    for name in [
        "City Watch (Manual copy 012345abcd)",
        "City Watch (Manual copy 012345abcd) (2)",
    ] {
        let directory = format!("/media/{name}");
        assert_eq!(show_directory_identity(&directory), base);
        assert!(directory_matches_show_path(
            &format!("{directory}/Season 1/City.Watch.S01E01.mkv"),
            base
        ));
        let (lower, upper) = show_directory_path_bounds(base).expect("family bounds");
        let path = format!("{directory}/Season 1/City.Watch.S01E01.mkv");
        assert!(path >= lower && path < upper);
    }
    for name in [
        "City Watch (Manual copy 012345abc)",
        "City Watch (Manual copy 012345ABCD)",
        "City Watch (Manual copy 012345abcd) (1)",
        "City Watch (Manual copy 012345abcd) (02)",
        "City Watch (Manual copy 012345abcd) extra",
        "City Watch (Extended)",
        "City Watch (Manual copy 012345abcd) (Manual copy abcdef0123)",
    ] {
        let directory = format!("/media/{name}");
        assert_eq!(show_directory_identity(&directory), directory);
        assert!(!directory_matches_show_path(
            &format!("{directory}/Season 1/City.Watch.S01E01.mkv"),
            base
        ));
    }
    assert_ne!(
        show_directory_identity("/other/City Watch (Manual copy 012345abcd)"),
        base
    );
}
