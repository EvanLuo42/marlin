//! The boot configuration: TOML, command-line overrides, and what they mount.

use marlin_framework::{BootError, ContentSettings, boot, register_cvars};

#[test]
fn boot_file_and_command_line() {
    register_cvars();
    let config = r#"
[content]
package = "build/game.mpak; dlc/extra.mpak; build/{platform}.mpak"

[game]
level = "01992d00-0000-7000-8000-000000000001"
"#;
    let errors = marlin_config::apply_config(config, marlin_config::Source::Project);
    assert!(errors.is_empty(), "{errors:?}");
    let errors = marlin_config::apply_command_line(["+content.cookd", "127.0.0.1:47200"]);
    assert!(errors.is_empty(), "{errors:?}");
    let settings = ContentSettings::from_cvars("C:/games/sample").unwrap();
    assert_eq!(
        settings.packages,
        [
            "C:/games/sample/build/game.mpak".to_owned(),
            "C:/games/sample/dlc/extra.mpak".to_owned(),
            format!("C:/games/sample/build/{}.mpak", marlin_rt::PLATFORM),
        ]
    );
    assert_eq!(settings.cookd, Some("127.0.0.1:47200".parse().unwrap()));
    assert_eq!(settings.platform, marlin_rt::PLATFORM);
    assert_eq!(
        settings.level,
        "01992d00-0000-7000-8000-000000000001".parse().unwrap()
    );

    // Mounting a package that is not there names it.
    match boot("", "C:/games/sample", ["+content.cookd", ""]) {
        Err(BootError::Package { path, .. }) => {
            assert!(path.ends_with("build/game.mpak"), "{path}")
        }
        Err(other) => panic!("{other}"),
        Ok(_) => panic!("mounted a missing package"),
    }
    // A bad level and bad syntax are reported, not ignored.
    assert!(matches!(
        boot("", "", ["+game.level", "nope"]),
        Err(BootError::Level(_))
    ));
    assert!(matches!(
        boot("[content\n", "", std::iter::empty()),
        Err(BootError::Config(_))
    ));
}
