//! The permission matrix suite. See `ruum_testkit::permissions` and `docs/TESTING.md`.

use ruum_testkit::permissions::{matrix_path, render_matrix};

#[tokio::test]
async fn permission_matrix() {
    ruum_testkit::permissions::run().await.finish();
}

/// The committed table must match the rules, so a rule change shows up in review.
#[test]
fn matrix_file_is_up_to_date() {
    let want = render_matrix();
    let path = matrix_path();
    if std::env::var("RUUM_UPDATE_MATRIX").as_deref() == Ok("1") {
        std::fs::write(&path, &want).expect("write permission_matrix.md");
        return;
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        have == want,
        "{} is out of date with the rules in src/permissions.rs. \
         Run `RUUM_UPDATE_MATRIX=1 cargo test -p ruum-testkit --test permissions` \
         and review the diff.",
        path.display()
    );
}

/// Reads of a trashed file and the changes feed follow the trash rule (`run_trashed`).
#[tokio::test]
async fn trashed_file_reads() {
    ruum_testkit::permissions::run_trashed().await.finish();
}
