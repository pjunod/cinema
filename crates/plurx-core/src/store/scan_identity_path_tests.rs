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
