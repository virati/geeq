// Pulling the camera raws that belong to tagged JPEGs off the memory card.
//
// geeq views JPEGs that were already copied to the computer, so a tagged
// path points at the computer, not at the card. The matching raw is found
// by name instead of by path: the card's DCIM folders are searched for a
// raw file with the same stem (case-insensitive), trying the folder the
// JPEG sits in first (Lumix `139_PANA`, Sony `100MSDCF` keep their folder
// names on the card and in the copy, but a renamed folder still works).
// The raw is copied next to the JPEG, never over an existing file.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::is_raw_file;

/// Where camera cards get mounted, per user (udisks, then older automounters).
fn mount_parents() -> Vec<PathBuf> {
    let user = std::env::var("USER").unwrap_or_default();
    vec![
        PathBuf::from("/run/media").join(&user),
        PathBuf::from("/media").join(&user),
        PathBuf::from("/media"),
    ]
}

/// The child of `dir` named `name`, ignoring case.
fn child_named(dir: &Path, name: &str) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
        .map(|e| e.path())
}

/// `root` itself if it is a DCIM folder, else the DCIM folder inside it.
fn dcim_in(root: &Path) -> Option<PathBuf> {
    if root.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("DCIM")) {
        return Some(root.to_path_buf());
    }
    child_named(root, "DCIM").filter(|p| p.is_dir())
}

/// The DCIM folders of every mounted camera card. `card_root` (the card's
/// mount point, or its DCIM folder) replaces the search when given.
pub fn card_dcims(card_root: Option<&Path>) -> Vec<PathBuf> {
    if let Some(root) = card_root {
        return dcim_in(root).into_iter().collect();
    }
    let mut found = Vec::new();
    for parent in mount_parents() {
        let Ok(entries) = fs::read_dir(&parent) else { continue };
        for entry in entries.flatten() {
            if let Some(dcim) = dcim_in(&entry.path()) {
                if !found.contains(&dcim) {
                    found.push(dcim);
                }
            }
        }
    }
    found.sort();
    found
}

/// The raw files on the card, listed lazily one folder at a time (a card
/// folder holds hundreds of files, and a pull usually touches few folders).
pub struct CardIndex {
    /// Every folder directly inside a DCIM folder, plus the DCIM folders.
    dirs: Vec<PathBuf>,
    /// Folder -> lowercase file stem -> raw file in it.
    listed: HashMap<PathBuf, HashMap<String, PathBuf>>,
}

impl CardIndex {
    pub fn new(dcims: Vec<PathBuf>) -> Self {
        let mut dirs = Vec::new();
        for dcim in &dcims {
            let mut subdirs: Vec<PathBuf> = fs::read_dir(dcim)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            subdirs.sort();
            dirs.extend(subdirs);
        }
        dirs.extend(dcims);
        CardIndex { dirs, listed: HashMap::new() }
    }

    fn raws_in(&mut self, dir: &Path) -> &HashMap<String, PathBuf> {
        self.listed.entry(dir.to_path_buf()).or_insert_with(|| {
            let mut raws = HashMap::new();
            for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if !is_raw_file(&path) || !path.is_file() {
                    continue;
                }
                if let Some(stem) = path.file_stem() {
                    raws.insert(stem.to_string_lossy().to_lowercase(), path);
                }
            }
            raws
        })
    }

    /// The raw on the card that goes with `photo` (same name, a raw
    /// extension), looking in the card folder named like the photo's own
    /// folder first.
    pub fn find(&mut self, photo: &Path) -> Option<PathBuf> {
        let stem = photo.file_stem()?.to_string_lossy().to_lowercase();
        let folder = photo
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_lowercase());
        let mut dirs = self.dirs.clone();
        dirs.sort_by_key(|d| {
            let name = d.file_name().map(|n| n.to_string_lossy().to_lowercase());
            name.is_none() || name != folder
        });
        dirs.iter().find_map(|dir| self.raws_in(dir).get(&stem).cloned())
    }
}

#[derive(Default, Debug)]
pub struct Summary {
    pub copied: usize,
    /// The raw was already next to the photo.
    pub existed: usize,
    /// The tagged file is itself a raw, so there is nothing to pull.
    pub already_raw: usize,
    /// Names of photos with no raw on the card.
    pub missing: Vec<String>,
    /// "name: reason" for raws that could not be copied.
    pub failed: Vec<String>,
}

impl Summary {
    /// One line for the tag popup.
    pub fn describe(&self, tag: usize) -> String {
        let mut text = format!("Tag {tag}: copied {} raw{}", self.copied, if self.copied == 1 { "" } else { "s" });
        if self.existed > 0 {
            text += &format!(", {} already there", self.existed);
        }
        if self.already_raw > 0 {
            text += &format!(", {} already raw", self.already_raw);
        }
        if !self.missing.is_empty() {
            let shown: Vec<&str> = self.missing.iter().take(3).map(String::as_str).collect();
            let more = self.missing.len() - shown.len();
            text += &format!(". Not on the card: {}", shown.join(", "));
            if more > 0 {
                text += &format!(" +{more} more");
            }
        }
        if !self.failed.is_empty() {
            text += &format!(". Failed: {}", self.failed.join("; "));
        }
        text
    }
}

/// Copies the raw of every photo in `photos` from the card to the photo's
/// own folder. `progress` gets the number of photos handled so far.
pub fn pull(photos: &[PathBuf], index: &mut CardIndex, mut progress: impl FnMut(usize)) -> Summary {
    let mut summary = Summary::default();
    for (i, photo) in photos.iter().enumerate() {
        pull_one(photo, index, &mut summary);
        progress(i + 1);
    }
    summary
}

fn pull_one(photo: &Path, index: &mut CardIndex, summary: &mut Summary) {
    let name = photo.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if is_raw_file(photo) {
        summary.already_raw += 1;
        return;
    }
    let Some(raw) = index.find(photo) else {
        summary.missing.push(name);
        return;
    };
    let (Some(dir), Some(raw_name)) = (photo.parent(), raw.file_name()) else {
        summary.missing.push(name);
        return;
    };
    let dest = dir.join(raw_name);
    if dest.exists() {
        summary.existed += 1;
        return;
    }
    // Copy under a temporary name so an interrupted pull never leaves a
    // truncated raw that looks complete.
    let tmp = dir.join(format!(".{}.part", raw_name.to_string_lossy()));
    let result = fs::copy(&raw, &tmp).and_then(|_| fs::rename(&tmp, &dest));
    match result {
        Ok(()) => summary.copied += 1,
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            summary.failed.push(format!("{}: {err}", raw_name.to_string_lossy()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh scratch folder holding a fake card (`card/DCIM/...`) and a
    /// fake computer folder (`disk/...`).
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("geeq_rawpull_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(path: &Path, bytes: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn finds_raw_by_name_in_the_matching_folder() {
        let root = scratch("match");
        touch(&root.join("card/DCIM/139_PANA/P1390001.RW2"), "right");
        touch(&root.join("card/DCIM/140_PANA/P1390001.RW2"), "other folder");
        let mut index = CardIndex::new(card_dcims(Some(&root.join("card"))));
        let found = index.find(&root.join("disk/Sep/139_PANA/p1390001.JPG")).unwrap();
        assert_eq!(fs::read_to_string(found).unwrap(), "right");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn falls_back_to_any_card_folder_when_the_copy_was_renamed() {
        let root = scratch("renamed");
        touch(&root.join("card/dcim/100MSDCF/DSC01234.ARW"), "arw");
        let mut index = CardIndex::new(card_dcims(Some(&root.join("card"))));
        let found = index.find(&root.join("disk/trip/DSC01234.JPG")).unwrap();
        assert_eq!(found.file_name().unwrap(), "DSC01234.ARW");
        assert!(index.find(&root.join("disk/trip/DSC09999.JPG")).is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pull_copies_beside_the_photo_and_reports_the_rest() {
        let root = scratch("pull");
        touch(&root.join("card/DCIM/100MSDCF/DSC00001.ARW"), "one");
        touch(&root.join("card/DCIM/100MSDCF/DSC00002.ARW"), "two (card)");
        touch(&root.join("disk/DSC00001.JPG"), "jpg");
        touch(&root.join("disk/DSC00002.JPG"), "jpg");
        touch(&root.join("disk/DSC00002.ARW"), "two (already copied)");
        touch(&root.join("disk/DSC00003.JPG"), "jpg");
        touch(&root.join("disk/DSC00004.ARW"), "raw");
        let photos: Vec<PathBuf> = ["DSC00001.JPG", "DSC00002.JPG", "DSC00003.JPG", "DSC00004.ARW"]
            .iter()
            .map(|n| root.join("disk").join(n))
            .collect();
        let mut index = CardIndex::new(card_dcims(Some(&root.join("card"))));
        let mut last = 0;
        let summary = pull(&photos, &mut index, |done| last = done);

        assert_eq!(last, 4);
        assert_eq!(summary.copied, 1);
        assert_eq!(summary.existed, 1);
        assert_eq!(summary.already_raw, 1);
        assert_eq!(summary.missing, vec!["DSC00003.JPG".to_string()]);
        assert!(summary.failed.is_empty());
        assert_eq!(fs::read_to_string(root.join("disk/DSC00001.ARW")).unwrap(), "one");
        // An existing raw is never overwritten, and no .part file is left.
        assert_eq!(fs::read_to_string(root.join("disk/DSC00002.ARW")).unwrap(), "two (already copied)");
        assert!(!root.join("disk/.DSC00001.ARW.part").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn card_root_may_be_the_dcim_folder_itself() {
        let root = scratch("dcim");
        touch(&root.join("card/DCIM/100MSDCF/DSC00001.ARW"), "x");
        assert_eq!(card_dcims(Some(&root.join("card/DCIM"))).len(), 1);
        assert_eq!(card_dcims(Some(&root.join("card"))).len(), 1);
        assert!(card_dcims(Some(&root.join("nowhere"))).is_empty());
        let _ = fs::remove_dir_all(&root);
    }
}
