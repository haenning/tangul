//! Boards as JSON files (`tangul_core::board`), one per board, in the `boards`
//! folder next to `settings.json`: easy to share, and to copy between databases
//! with the same schema. A file keeps its name when the board is renamed; a new
//! board gets one from its name. Without a folder (tests) boards live in memory.

use std::path::{Path, PathBuf};

use tangul_core::board::Board;

const DIR: &str = "boards";

/// The boards, loaded once and written through on every change.
#[derive(Debug, Default)]
pub struct Boards {
    dir: Option<PathBuf>,
    /// Each board with the file it lives in (`None` in memory).
    boards: Vec<(Board, Option<PathBuf>)>,
    /// Files that didn't load, and why.
    problems: Vec<String>,
}

impl Boards {
    /// The boards in `config_dir`'s `boards` folder; `None` keeps them in memory.
    pub fn open(config_dir: Option<&Path>) -> Self {
        let mut boards = Self {
            dir: config_dir.map(|d| d.join(DIR)),
            ..Self::default()
        };
        boards.reload();
        boards
    }

    /// Reads the folder again (files changed outside tangul).
    pub fn reload(&mut self) {
        self.boards.clear();
        self.problems.clear();
        let Some(dir) = &self.dir else { return };
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        files.sort();
        for path in files {
            let loaded = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|text| serde_json::from_str::<Board>(&text).map_err(|e| e.to_string()));
            match loaded {
                Ok(board) if self.get(&board.id).is_some() => self.problems.push(format!(
                    "{}: another file has the board id {}",
                    path.display(),
                    board.id
                )),
                Ok(board) => self.boards.push((board, Some(path))),
                Err(e) => self.problems.push(format!("{}: {e}", path.display())),
            }
        }
    }

    /// Files that didn't load, and why.
    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    pub fn get(&self, id: &str) -> Option<&Board> {
        self.boards.iter().map(|(b, _)| b).find(|b| b.id == id)
    }

    /// A database's boards, archived ones too, by name.
    pub fn for_database(&self, database_id: &str) -> Vec<&Board> {
        let mut out: Vec<&Board> = self
            .boards
            .iter()
            .map(|(b, _)| b)
            .filter(|b| b.database_id == database_id)
            .collect();
        out.sort_by_cached_key(|b| (b.name.to_lowercase(), b.id.clone()));
        out
    }

    /// Adds `board`, or replaces the one with its id, and writes its file.
    pub fn save(&mut self, board: Board) -> Result<(), String> {
        let at = self.boards.iter().position(|(b, _)| b.id == board.id);
        let path = match (&self.dir, at) {
            (None, _) => None,
            (Some(_), Some(i)) => self.boards[i].1.clone(),
            (Some(dir), None) => Some(free_path(dir, &board.name)),
        };
        if let Some(path) = &path {
            write(path, &board)?;
        }
        match at {
            Some(i) => self.boards[i] = (board, path),
            None => self.boards.push((board, path)),
        }
        Ok(())
    }

    /// Removes a board and its file. Unknown ids are fine.
    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        let Some(at) = self.boards.iter().position(|(b, _)| b.id == id) else {
            return Ok(());
        };
        if let Some(path) = &self.boards[at].1 {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        self.boards.remove(at);
        Ok(())
    }
}

fn write(path: &Path, board: &Board) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(board).map_err(|e| e.to_string())?;
    std::fs::write(path, json + "\n").map_err(|e| e.to_string())
}

/// `signups-after-launch.json` for "Signups after launch!", with `-2`, `-3`, … if
/// taken.
fn free_path(dir: &Path, name: &str) -> PathBuf {
    let stem = slug(name);
    (1..)
        .map(|n| match n {
            1 => dir.join(format!("{stem}.json")),
            n => dir.join(format!("{stem}-{n}.json")),
        })
        .find(|p| !p.exists())
        .expect("a free file name")
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.chars().count() >= 40 {
            break;
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "board".into()
    } else {
        out.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tangul_core::board::{Refresh, Tile};

    fn board(id: &str, name: &str, db: &str) -> Board {
        Board {
            id: id.into(),
            name: name.into(),
            database_id: db.into(),
            ..Default::default()
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tangul-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn names_become_file_names() {
        assert_eq!(slug("Signups after launch!"), "signups-after-launch");
        assert_eq!(slug("  Ärger: Q3 / Q4  "), "ärger-q3-q4");
        assert_eq!(slug("!!!"), "board");
        assert_eq!(slug(""), "board");
        assert_eq!(slug(&"x".repeat(100)).len(), 40);
    }

    #[test]
    fn in_memory_boards_are_kept_and_listed_per_database() {
        let mut boards = Boards::open(None);
        boards.save(board("1", "beta", "db")).unwrap();
        boards.save(board("2", "Alpha", "db")).unwrap();
        boards.save(board("3", "other", "db2")).unwrap();
        let names = |b: &Boards, db| -> Vec<String> {
            b.for_database(db).iter().map(|b| b.name.clone()).collect()
        };
        assert_eq!(names(&boards, "db"), ["Alpha", "beta"]);
        boards.save(board("1", "Gamma", "db")).unwrap();
        assert_eq!(names(&boards, "db"), ["Alpha", "Gamma"]);
        boards.delete("2").unwrap();
        boards.delete("missing").unwrap();
        assert_eq!(names(&boards, "db"), ["Gamma"]);
        assert!(boards.get("3").is_some());
    }

    #[test]
    fn boards_are_saved_to_and_loaded_from_the_folder() {
        let config = temp_dir("boards");
        let dir = config.join(DIR);
        let mut boards = Boards::open(Some(&config));
        assert!(boards.for_database("db").is_empty());

        let mut first = board("1", "Signups", "db");
        first.refresh = Refresh::Manual;
        first.tiles.push(Tile {
            id: "t".into(),
            title: "Per day".into(),
            sql: "select 1".into(),
            ..Default::default()
        });
        boards.save(first.clone()).unwrap();
        boards.save(board("2", "Signups", "db")).unwrap();
        assert!(dir.join("signups.json").exists());
        assert!(dir.join("signups-2.json").exists());

        // Renamed: same file.
        first.name = "Growth".into();
        boards.save(first.clone()).unwrap();
        assert!(!dir.join("growth.json").exists());

        // A broken file and a copy with a taken id are reported, not loaded.
        std::fs::write(dir.join("broken.json"), "{").unwrap();
        std::fs::copy(dir.join("signups.json"), dir.join("zz-copy.json")).unwrap();
        std::fs::write(dir.join("notes.txt"), "not a board").unwrap();

        let mut again = Boards::open(Some(&config));
        assert_eq!(again.get("1"), Some(&first));
        assert_eq!(again.for_database("db").len(), 2);
        assert_eq!(again.problems().len(), 2, "{:?}", again.problems());
        assert!(again.problems()[0].contains("broken.json"));
        assert!(again.problems()[1].contains("another file has the board id 1"));

        again.delete("2").unwrap();
        assert!(!dir.join("signups-2.json").exists());
        again.reload();
        assert!(again.get("2").is_none());
        std::fs::remove_dir_all(&config).unwrap();
    }

    #[test]
    fn a_file_deleted_outside_is_no_error() {
        let config = temp_dir("boards-gone");
        let mut boards = Boards::open(Some(&config));
        boards.save(board("1", "x", "db")).unwrap();
        std::fs::remove_file(config.join(DIR).join("x.json")).unwrap();
        boards.delete("1").unwrap();
        assert!(boards.get("1").is_none());
        std::fs::remove_dir_all(&config).unwrap();
    }
}
