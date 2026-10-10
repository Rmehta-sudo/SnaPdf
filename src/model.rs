use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppState {
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub recent_documents: Vec<RecentDocument>,
    #[serde(default)]
    pub last_document: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub root: PathBuf,
    #[serde(default = "default_true")]
    pub expanded: bool,
    #[serde(default)]
    pub recent_documents: Vec<RecentDocument>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentDocument {
    pub path: PathBuf,
    pub opened_at: u64,
}

fn default_true() -> bool {
    true
}

impl Project {
    pub fn from_folder(root: PathBuf) -> Self {
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Project")
            .to_owned();

        Self {
            name,
            root,
            expanded: true,
            recent_documents: Vec::new(),
        }
    }

    pub fn pdf_files(&self) -> Vec<PathBuf> {
        let mut files: Vec<_> = WalkDir::new(&self.root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| {
                !entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with('.') && entry.depth() > 0)
            })
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file() && is_pdf(entry.path()))
            .map(|entry| entry.into_path())
            .collect();

        files.sort_by_key(|path| {
            path.strip_prefix(&self.root)
                .unwrap_or(path)
                .to_string_lossy()
                .to_lowercase()
        });
        files
    }
}

impl AppState {
    pub fn add_project(&mut self, root: PathBuf) -> bool {
        if self.projects.iter().any(|project| project.root == root) {
            return false;
        }
        self.projects.push(Project::from_folder(root));
        true
    }

    pub fn record_open(&mut self, path: &Path) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let recent = RecentDocument {
            path: path.to_path_buf(),
            opened_at: now,
        };

        upsert_recent(&mut self.recent_documents, recent.clone(), 15);
        if let Some(project) = self
            .projects
            .iter_mut()
            .filter(|project| path.starts_with(&project.root))
            .max_by_key(|project| project.root.components().count())
        {
            upsert_recent(&mut project.recent_documents, recent, 8);
        }
        self.last_document = Some(path.to_path_buf());
    }

    pub fn prune_missing(&mut self) {
        self.projects.retain(|project| project.root.is_dir());
        self.recent_documents
            .retain(|document| document.path.is_file());
        for project in &mut self.projects {
            project
                .recent_documents
                .retain(|document| document.path.is_file());
        }
        if self
            .last_document
            .as_ref()
            .is_some_and(|path| !path.is_file())
        {
            self.last_document = None;
        }
    }
}

pub fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

fn upsert_recent(items: &mut Vec<RecentDocument>, item: RecentDocument, limit: usize) {
    items.retain(|existing| existing.path != item.path);
    items.insert(0, item);
    items.truncate(limit);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_detection_is_case_insensitive() {
        assert!(is_pdf(Path::new("paper.PDF")));
        assert!(is_pdf(Path::new("paper.pdf")));
        assert!(!is_pdf(Path::new("notes.txt")));
    }

    #[test]
    fn duplicate_projects_are_rejected() {
        let mut state = AppState::default();
        let root = PathBuf::from("/tmp/course");
        assert!(state.add_project(root.clone()));
        assert!(!state.add_project(root));
    }
}
