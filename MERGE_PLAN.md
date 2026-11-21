I reviewed the plan five times, each round fixing the biggest gap the last one left. I've changed nothing in the repo yet. The final plan is at the bottom.

**What I'm working from**
1. Your memory notes: you check the GUI yourself, so I verify in code and builds and keep `/home/virati/.cargo/bin/geeq` current. Panels should be resizable and hideable, with no wasted space. No side benchmarks.
2. `code-bases`: shortest code that keeps behavior, no new files, no shims.
3. Your control-theory way of thinking. Treat the repo as a system: a target state, sensors that show where it really is, small steps you can watch one at a time, and a fix for whatever keeps causing the error so it stops coming back. Your open PRs (Lie brackets, feedback linearization) are why I read your goals this way.

**Round 1: is the check strong enough?**
- Problem: "every added line is still there" is a weak check. A feature can be lost by deleting lines, and that's exactly how the two losses got in. Kept lines also don't prove the program still works.
- Fix: three checks before and after the merge:
  1. Every added line is kept.
  2. Every removed line is either accounted for or on a written list of known losses.
  3. `cargo test` passes.
- Also write the list of losses into the PR description, so the record isn't only in this chat.

**Round 2: one change at a time**
- Problem: PR #2 mixes three things: the merge, the new right-click behavior, and the `geeq`/`cgeeq` split. You haven't tried the right-click change yet, so merging it now would put an unchecked change on `main`.
- Fix:
  1. Each change is its own commit.
  2. You try the right-click menu with `/home/virati/.cargo/bin/geeq` before anything goes to `main`.
  3. If it needs work, that happens on the branch, not after the merge.

**Round 3: can each step be undone?**
- Problem: deleting branches, especially on GitHub, is the only step that's hard to undo.
- Fix:
  1. The merge commit keeps every branch's commits inside `main`.
  2. PR #2 lists the old branch tips (`7b89250`, `39394fc`, `63d9d59`, `9daf55a`) so any of them can be brought back.
  3. Branches are deleted with `git branch -d`, which refuses to delete anything not merged.
  4. Reinstall from `main` after the merge, so the installed binary matches `main`.

**Round 4: simple enough?**
- Problem: having the binary check its own name is a hidden trick. `cargo run` always gives `geeq` behavior, and `cargo uninstall` would leave a broken `cgeeq` link. I also weighed:
  1. A `--condensed` flag with a shell alias: the setup lives in your `.zshrc` instead of the repo.
  2. Two Cargo binaries: tested, cargo warns and builds everything twice.
- Fix: keep the name check. It's one line next to the existing `--video` check, needs no zshrc edit, and adds no file. The README states the one-time `ln` step plainly.
- Part 2 check: adding Rotate left to the menu makes the ring busier. Showing Reset only when the photo is rotated keeps the ring at its current size in the common case.

**Round 5: why did this become a mess?**
- Root cause:
  1. Branches were stacked on unmerged branches.
  2. PR #1 sat open for 7 days.
  3. The README was written on a side branch and had a false claim.
  4. A big UI rewrite dropped two features without saying so.
- Fix so it doesn't recur:
  1. One branch per feature, off `main`, merged or closed before the next one starts.
  2. Any commit that removes UI lists what it removed.
  3. I'd save this as a project memory note so later sessions follow it.

**The plan after these changes stopped changing**
1. On `exif-camera-top`, commit the right-click change by itself.
2. You try it (`geeq` is already installed with it) and say OK, or I fix it on the branch.
3. Merge `origin/main` into `exif-camera-top` (history only, no file changes).
4. One commit: the `cgeeq` name check, which starts with the Files panel hidden. Plain `geeq` opens with Files shown.
5. One commit: fix `README.md` (remove the false `cgeeq` package claim, and cover `geeq` vs `cgeeq`, the `ln` step, `--video` and the hold-and-release menu).
6. Run `cargo build --release` and `cargo test`. Rerun the added-line and removed-line checks; the only losses should be the two known ones.
7. Push, then open PR #2 (`exif-camera-top` → `main`), listing the two losses and the old branch tips. Add it to `prdash`.
8. Merge PR #2 with a merge commit. Confirm PR #1 shows as merged, or close it with a pointer to #2. Refresh `prdash`.
9. Update the local `main`, reinstall, and run `ln -sf geeq /home/virati/.cargo/bin/cgeeq`.
10. Confirm each old branch is contained in `main`, then delete `streamlined`, `exif-camera-top` and `readme_update` locally and on GitHub.
11. Save the branch rule as a memory note.
12. Then, one at a time off `main`:
    1. `restore-start-in-grid`
    2. `restore-rotate-left-reset`

**Needs your go**
1. Step 2: try the right-click menu now and tell me if it feels right.
2. Steps 7, 8 and 10 change GitHub. Say go and I'll do them as listed.
