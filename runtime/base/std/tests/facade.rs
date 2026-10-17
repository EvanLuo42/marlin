//! The facade exposes every base crate and a working prelude.

use marlin_std::prelude::*;

#[test]
fn prelude_and_reexports_work() {
    let shared: Arc<Vec<String>> = Arc::new(vec!["a".to_string(), format!("{}", 1)]);
    let local = Rc::new(Box::new(shared.len()));
    assert_eq!(**local, 2);

    let _ = marlin_std::time::Instant::now();
    let _ = marlin_std::time::SystemTime::now().to_utc();
    let mutex = marlin_std::sync::Mutex::new(1);
    *mutex.lock() += 1;
    assert_eq!(*mutex.lock(), 2);
    assert_eq!(marlin_std::process::id(), std::process::id());
    assert!(marlin_std::io::fs::exists(
        std::env::temp_dir().to_str().unwrap()
    ));
    assert!(marlin_std::mem::system_memory().unwrap().total_physical > 0);
    marlin_std::diag::info!("facade test");
}
