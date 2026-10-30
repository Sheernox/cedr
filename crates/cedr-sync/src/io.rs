//! Contained paths and atomic writes; neither source nor destination symlinks are followed.
use crate::Result;
use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};
pub fn path(root: &Path, relative: &str) -> Result<PathBuf> {
    let rel = Path::new(relative);
    if relative.is_empty()
        || rel.is_absolute()
        || relative.contains('\\')
        || rel.components().any(|p| !matches!(p, Component::Normal(_)))
    {
        return Err("sync: invalid relative path".into());
    }
    let mut p = root.to_path_buf();
    for component in rel.components() {
        p.push(component);
        match fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(format!("sync: symlink refused: {relative}"));
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(format!("sync: cannot inspect {relative}")),
        }
    }
    Ok(p)
}
pub fn read(root: &Path, rel: &str) -> Result<Option<Vec<u8>>> {
    let p = path(root, rel)?;
    match fs::read(p) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(format!("sync: cannot read {rel}")),
    }
}
pub fn write(root: &Path, rel: &str, bytes: &[u8]) -> Result<()> {
    let p = path(root, rel)?;
    let parent = p.parent().ok_or("sync: missing parent")?;
    fs::create_dir_all(parent).map_err(|_| format!("sync: cannot create parent of {rel}"))?;
    path(root, rel)?;
    let mut temp =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| format!("sync: cannot stage {rel}"))?;
    temp.write_all(bytes)
        .map_err(|_| format!("sync: cannot write {rel}"))?;
    temp.persist(p)
        .map_err(|_| format!("sync: cannot replace {rel}"))?;
    Ok(())
}
pub fn files(root: &Path, rel: &str) -> Result<Vec<String>> {
    let p = path(root, rel)?;
    if !p.exists() {
        return Ok(vec![]);
    }
    let mut result = vec![];
    for entry in fs::read_dir(p).map_err(|_| format!("sync: cannot list {rel}"))? {
        let entry = entry.map_err(|_| format!("sync: cannot list {rel}"))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "sync: non UTF-8 path")?;
        let child = format!("{rel}/{name}");
        let p = path(root, &child)?;
        if p.is_dir() {
            result.extend(files(root, &child)?);
        } else {
            result.push(child);
        }
    }
    result.sort();
    Ok(result)
}
