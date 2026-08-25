//! Adversarial symlink coverage for the cleaner engine.
//!
//! Every test pins one invariant: a link entry is always treated as a single
//! unlink-only deletion unit — never descended into, never sized by its
//! target and never deleted through. Payloads living OUTSIDE the cleaned
//! root must survive every operation byte-for-byte.

use std::fs;

use diskpulse::cleaner::CleanOptions;
use diskpulse::cleaner::{create_clean_plan, execute_clean_plan};
use diskpulse::models::CleanPlan;

mod common;
use common::TestWorkspace;

fn opts_for(root: &std::path::Path) -> CleanOptions {
    CleanOptions {
        custom_path: Some(root.to_path_buf()),
        apply: true,
        yes: true,
        ..CleanOptions::default()
    }
}

fn planned(plan: &CleanPlan) -> Vec<std::path::PathBuf> {
    plan.items.iter().map(|item| item.path.clone()).collect()
}

#[test]
fn directory_symlink_loop_terminates_and_plans_link_only() {
    let ws = TestWorkspace::new();
    let cache = ws.create_dir("cache");
    ws.create_file("cache/payload.bin", 4_096);
    // Self-referential cycle inside the cleaned root.
    let _loop_link = ws.create_symlink("cache", "cache/loop").expect("symlink");

    let plan = create_clean_plan(&opts_for(&cache)).expect("plan must terminate");
    let item = plan
        .items
        .iter()
        .find(|i| i.path.ends_with("loop"))
        .expect("the link itself must appear exactly once");
    assert!(!item.is_dir, "link must not be classified as a dir");
    assert_eq!(
        plan.total_items,
        2,
        "payload + link only: {:?}",
        planned(&plan)
    );

    let report = execute_clean_plan(&plan, false).expect("execute");
    assert_eq!(report.errors_count, 0);
    // Both direct children are deletion units; the link must be UNLINKED,
    // never recursed into (which would trip the self-referential cycle).
    assert_eq!(report.items_freed, 2);
    assert!(!cache.join("loop").exists(), "link unlinked");
    assert!(cache.is_dir(), "root itself stays");
}

#[test]
fn external_target_of_dir_symlink_is_never_descended_or_deleted() {
    let ws = TestWorkspace::new();
    let cache = ws.create_dir("cache");
    let outside = ws.create_dir("outside");
    fs::write(outside.join("precious.txt"), b"keep me").expect("write");

    let link = ws.create_symlink("outside", "cache/ext").expect("symlink");

    let plan = create_clean_plan(&opts_for(&cache)).expect("plan");
    let ext_item = plan
        .items
        .iter()
        .find(|i| i.path == link)
        .expect("dir symlink planned as a single unit");
    assert!(!ext_item.is_dir);
    assert!(
        ext_item.size < 4096,
        "size came from the target subtree: {}",
        ext_item.size
    );

    let report = execute_clean_plan(&plan, false).expect("execute");
    assert_eq!(report.errors_count, 0);
    assert!(!link.exists(), "link removed");
    assert!(
        outside.join("precious.txt").exists(),
        "external payload must survive"
    );
}

#[test]
fn file_symlink_is_sized_as_a_link_not_its_target() {
    let ws = TestWorkspace::new();
    let cache = ws.create_dir("cache");
    let outside = ws.create_dir("outside");
    let big = outside.join("big.dat");
    fs::write(&big, vec![b'x'; 65_536]).expect("write");

    let link = ws
        .create_symlink("../outside/big.dat", "cache/big.link")
        .expect("symlink");

    let plan = create_clean_plan(&opts_for(&cache)).expect("plan");
    let item = plan.items.iter().find(|i| i.path == link).expect("planned");
    assert_ne!(item.size, 65_536, "target bytes leaked into the plan");

    execute_clean_plan(&plan, false).expect("execute");
    assert!(!link.exists());
    assert!(big.exists(), "target file must survive");
}

#[test]
fn custom_path_pointing_at_a_symlink_unlinks_only_the_link() {
    let ws = TestWorkspace::new();
    ws.create_dir("outside");
    let target = ws.join("outside/data.bin");
    fs::write(&target, vec![b'd'; 8_192]).expect("write");
    let link = ws
        .create_symlink("../outside/data.bin", "cache/data.link")
        .expect("symlink");

    let options = CleanOptions {
        custom_path: Some(link.clone()),
        apply: true,
        yes: true,
        ..CleanOptions::default()
    };
    let plan = create_clean_plan(&options).expect("plan");
    assert_eq!(plan.total_items, 1);
    assert!(!plan.items[0].is_dir);

    let report = execute_clean_plan(&plan, false).expect("execute");
    assert_eq!(report.items_freed, 1);
    assert!(!link.exists(), "link unlinked");
    assert!(target.exists(), "link target untouched");
}

#[test]
fn age_descent_into_fresh_dirs_skips_symlinked_subtrees() {
    let ws = TestWorkspace::new();
    let cache = ws.create_dir("cache");
    ws.create_dir("cache/fresh_pkg");
    ws.create_file("cache/fresh_pkg/stale.bin", 2_048);
    ws.set_age_seconds("cache/fresh_pkg/stale.bin", 4 * 3600);

    // Ancient content hidden behind a link inside the fresh package.
    ws.create_dir("outside_stash");
    ws.create_file("outside_stash/ancient.bin", 32_768);
    ws.set_age_seconds("outside_stash/ancient.bin", 30 * 24 * 3600);
    // Link lives two levels deep, so reaching the workspace-level stash
    // requires climbing two parents.
    let _link = ws
        .create_symlink("../../outside_stash", "cache/fresh_pkg/shortcut")
        .expect("symlink");

    let options = CleanOptions {
        custom_path: Some(cache.clone()),
        older_than: Some(chrono::Duration::hours(1)),
        apply: true,
        yes: true,
        ..CleanOptions::default()
    };

    let plan = create_clean_plan(&options).expect("plan");
    let planned = planned(&plan);

    assert!(
        planned.contains(&ws.join("cache/fresh_pkg/stale.bin")),
        "stale real file must be reached through descent: {planned:?}"
    );
    assert!(
        !planned.contains(&ws.join("outside_stash/ancient.bin")),
        "descent must never cross a symlink boundary: {planned:?}"
    );
    assert!(
        !planned.iter().any(|p| p.ends_with("outside_stash")),
        "external stash dir itself must never be a unit: {planned:?}"
    );

    let report = execute_clean_plan(&plan, false).expect("execute");
    assert_eq!(report.errors_count, 0);
    assert!(
        ws.join("outside_stash/ancient.bin").exists(),
        "external stash survived"
    );
    assert!(
        !ws.join("cache/fresh_pkg/stale.bin").exists(),
        "stale real file was deleted"
    );
}
