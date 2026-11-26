//! What reaches the index and the object store of the graph's repository:
//! files staged as their bytes on disk, blobs written from the files a
//! write staged, and index entries set from blob ids. Every byte goes in
//! as it is: the repository's own [`ATTRIBUTES`], checked before each
//! staging and each write of blobs, unset every attribute that converts,
//! and no other path reads a file through a filter.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

use super::super::encoding::{path, path_bytes};
use super::super::process::{run, shown};
use super::isolation::ATTRIBUTES;
use super::oid::Oid;
use super::repository::Repository;

impl Repository {
    /// Put `paths`, relative to the work tree, in the index as they are on
    /// disk: each file's bytes as they are, through no filter or line-end
    /// conversion; a path with no file is taken out, and one the index
    /// never held is no error.
    pub(crate) fn stage(&self, paths: &[PathBuf]) -> Result<()> {
        self.unset_conversions()?;
        let mut files = Vec::with_capacity(paths.len());
        let mut links = Vec::new();
        for relative in paths {
            let full = self.work_tree.join(relative);
            match fs::symlink_metadata(&full) {
                Ok(metadata) if metadata.file_type().is_symlink() => links.push(relative),
                Ok(_) => files.push(relative),
                Err(e) if e.kind() == ErrorKind::NotFound => files.push(relative),
                Err(source) => return Err(Error::Io { path: full, source }),
            }
        }
        if !files.is_empty() {
            self.update(&files)?;
        }
        if !links.is_empty() {
            self.add_through(&links)?;
        }
        Ok(())
    }

    /// Write the files `names` of the directory `dir` to the object store
    /// as blobs, each its bytes as they are, in one object transaction
    /// ([`OVERRIDES`](super::OVERRIDES)), and return their ids in the order
    /// of `names`. The ids are read back from `index`, a scratch index file
    /// this call writes and leaves to its caller to remove: the
    /// repository's own index and work tree are neither read nor written.
    pub(crate) fn write_blobs(
        &self,
        dir: &Path,
        names: &[&OsStr],
        index: &Path,
    ) -> Result<Vec<Oid>> {
        self.unset_conversions()?;
        let scratch = || {
            let mut command = self.command_in(dir);
            command.env("GIT_INDEX_FILE", index);
            command
        };
        let input: Vec<u8> = names
            .iter()
            .flat_map(|name| path_bytes(Path::new(name)).into_iter().chain([0]))
            .collect();
        let add = ["update-index", "-z", "--add", "--stdin"].map(OsStr::new);
        run(scratch(), dir, &add, Some(&input))?;
        let list = ["ls-files", "-z", "--stage"].map(OsStr::new);
        let stdout = run(scratch(), dir, &list, None)?;
        let unreadable = || Error::GitOutput {
            args: shown(&list),
            detail: "no entry of an object name for a file it was given",
        };
        let mut listed = stdout
            .split(|&byte| byte == 0)
            .filter(|record| !record.is_empty())
            .map(index_entry)
            .collect::<Option<BTreeMap<PathBuf, Oid>>>()
            .ok_or_else(unreadable)?;
        names
            .iter()
            .map(|name| listed.remove(Path::new(name)).ok_or_else(unreadable))
            .collect()
    }

    /// Set the index entry of each path of `entries`, relative to the work
    /// tree, to its blob as a regular file, or take the path out when its
    /// id is the null name. The work tree is not read.
    pub(crate) fn set_index(&self, entries: &[(&Path, &Oid)]) -> Result<()> {
        let mut info = Vec::new();
        for (path, oid) in entries {
            info.extend_from_slice(if oid.is_null() { b"0 " } else { b"100644 " });
            info.extend_from_slice(oid.as_bytes());
            info.push(b'\t');
            info.extend(path_bytes(path));
            info.push(0);
        }
        let args = ["update-index", "-z", "--index-info"].map(OsStr::new);
        self.run(&args, Some(&info)).map(drop)
    }

    /// Write [`ATTRIBUTES`] to the repository's `info/attributes` when it
    /// holds anything else, so no attribute converts what is staged or
    /// written as a blob, whatever a person did to the file.
    fn unset_conversions(&self) -> Result<()> {
        let attributes = self.git_dir.join("info").join("attributes");
        // Neither read nor written when it is no regular file: either one
        // would wait on a FIFO for its other end forever.
        match fs::metadata(&attributes) {
            Ok(metadata) if !metadata.is_file() => {
                return Err(Error::NotAFile { path: attributes });
            }
            Ok(_) | Err(_) => {}
        }
        match fs::read(&attributes) {
            Ok(held) if held == ATTRIBUTES.as_bytes() => return Ok(()),
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::Io {
                    path: attributes,
                    source,
                });
            }
        }
        let info = self.git_dir.join("info");
        fs::create_dir_all(&info).map_err(Error::io(&info))?;
        fs::write(&attributes, ATTRIBUTES).map_err(Error::io(&attributes))
    }

    /// Stage the files and the missing paths of `paths` in one process,
    /// in one object transaction: each file as its bytes, each missing
    /// path taken out.
    fn update(&self, paths: &[&PathBuf]) -> Result<()> {
        let input: Vec<u8> = paths
            .iter()
            .flat_map(|path| path_bytes(path).into_iter().chain([0]))
            .collect();
        let args = ["update-index", "-z", "--add", "--remove", "--stdin"].map(OsStr::new);
        self.run(&args, Some(&input)).map(drop)
    }

    /// Stage each of `links`, symlinks, as the bytes of the file it leads
    /// to, which a load reads, through no filter: `update-index` would
    /// stage the link itself.
    fn add_through(&self, links: &[&PathBuf]) -> Result<()> {
        let mut args: Vec<&OsStr> = ["hash-object", "-w", "--no-filters", "--"]
            .iter()
            .map(OsStr::new)
            .collect();
        args.extend(links.iter().map(|path| path.as_os_str()));
        let stdout = self.run(&args, None)?;
        let oids = stdout
            .split(|&byte| byte == b'\n')
            .filter(|oid| !oid.is_empty())
            .map(Oid::parse)
            .collect::<Option<Vec<Oid>>>()
            .filter(|oids| oids.len() == links.len())
            .ok_or_else(|| Error::GitOutput {
                args: shown(&args),
                detail: "no object name for each path",
            })?;
        let entries: Vec<(&Path, &Oid)> =
            links.iter().map(|link| link.as_path()).zip(&oids).collect();
        self.set_index(&entries)
    }
}

/// One record of `ls-files -z --stage`: `<mode> <object> <stage>\t<path>`.
fn index_entry(record: &[u8]) -> Option<(PathBuf, Oid)> {
    let tab = record.iter().position(|&byte| byte == b'\t')?;
    let (fields, name) = (record.get(..tab)?, record.get(tab + 1..)?);
    let object = fields.split(|&byte| byte == b' ').nth(1)?;
    Some((path(name), Oid::parse(object)?))
}
