use cold_vtl_core::MediaReadGuard;
use std::{
    fs::{self, File},
    os::unix::fs::symlink,
};

#[test]
fn export_blocks_same_cartridge_loading_but_not_other_media() {
    let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
    fs::create_dir(&root).unwrap();
    let a = root.join("TAPE01");
    let b = root.join("TAPE02");
    fs::create_dir(&a).unwrap();
    fs::create_dir(&b).unwrap();
    let api = MediaReadGuard::acquire(&a).unwrap();
    let worker = MediaReadGuard::acquire(&a).unwrap();
    let drive = File::open(&a).unwrap();
    assert!(drive.try_lock().is_err());
    File::open(&b).unwrap().try_lock().unwrap();
    drop(api);
    assert!(
        drive.try_lock().is_err(),
        "worker must outlive API protection"
    );
    drop(worker);
    drive.try_lock().unwrap();
    assert!(
        MediaReadGuard::acquire(&a).is_err(),
        "loaded tape cannot be exported"
    );
    drop(drive);
    assert!(MediaReadGuard::acquire(&a).is_ok());
    symlink(&a, root.join("LINK01")).unwrap();
    assert!(MediaReadGuard::acquire(&root.join("LINK01")).is_err());
    fs::write(root.join("FILE01"), b"not a directory").unwrap();
    assert!(MediaReadGuard::acquire(&root.join("FILE01")).is_err());
    fs::remove_dir_all(root).unwrap();
}
