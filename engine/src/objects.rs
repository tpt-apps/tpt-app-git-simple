//! Git object model: parsing of raw object payloads (blobs, trees, commits,
//! tags).

use crate::error::{GitError, Result};
use crate::oid::Oid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectType {
    Commit,
    Tree,
    Blob,
    Tag,
}

impl ObjectType {
    pub fn from_name(name: &[u8]) -> Option<Self> {
        match name {
            b"commit" => Some(ObjectType::Commit),
            b"tree" => Some(ObjectType::Tree),
            b"blob" => Some(ObjectType::Blob),
            b"tag" => Some(ObjectType::Tag),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            ObjectType::Commit => "commit",
            ObjectType::Tree => "tree",
            ObjectType::Blob => "blob",
            ObjectType::Tag => "tag",
        }
    }
}

/// One parsed object: its type and raw payload.
#[derive(Debug, Clone)]
pub struct Object {
    pub kind: ObjectType,
    pub data: Vec<u8>,
}

/// A file/dir entry inside a tree object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// ASCII octal as stored: `100644`, `100755`, `120000` (symlink),
    /// `160000` (submodule gitlink), `40000` (directory).
    pub mode: String,
    pub name: String,
    pub oid: Oid,
}

impl TreeEntry {
    pub fn is_dir(&self) -> bool {
        self.mode == "40000" || self.mode == "040000"
    }
}

/// A parsed commit object.
#[derive(Debug, Clone)]
pub struct Commit {
    pub tree: Oid,
    pub parents: Vec<Oid>,
    /// `(name, email, seconds, tz-offset-string)` of the author.
    pub author: Signature,
    pub committer: Signature,
    pub message: String,
}

/// `Name <email> 1739000000 +1300`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub name: String,
    pub email: String,
    pub time: i64,
    /// e.g. `+1300`.
    pub timezone: String,
}

impl Signature {
    pub fn parse(line: &str) -> Result<Self> {
        let lt = line
            .rfind('<')
            .ok_or_else(|| GitError::Corrupt(format!("bad signature line: {line}")))?;
        let gt = line
            .rfind('>')
            .ok_or_else(|| GitError::Corrupt(format!("bad signature line: {line}")))?;
        let name = line[..lt].trim().to_string();
        let email = line[lt + 1..gt].to_string();
        let rest = line[gt + 1..].trim();
        let mut parts = rest.split_whitespace();
        let time = parts
            .next()
            .and_then(|t| t.parse::<i64>().ok())
            .ok_or_else(|| GitError::Corrupt(format!("bad signature timestamp: {line}")))?;
        let timezone = parts.next().unwrap_or("+0000").to_string();
        Ok(Signature { name, email, time, timezone })
    }
}

/// Parses `"blob 1234\0<data>"`-style raw object framing.
pub fn parse_loose(bytes: &[u8]) -> Result<Object> {
    let inflated = crate::zlib::inflate(bytes)?;
    let nul = inflated
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| GitError::Corrupt("loose object missing NUL header terminator".into()))?;
    let header = &inflated[..nul];
    let header = std::str::from_utf8(header)
        .map_err(|_| GitError::Corrupt("loose object header not UTF-8".into()))?;
    let mut parts = header.splitn(2, ' ');
    let kind_name = parts.next().unwrap_or("");
    let kind = ObjectType::from_name(kind_name.as_bytes())
        .ok_or_else(|| GitError::Corrupt(format!("unknown object type {kind_name:?}")))?;
    let _declared_size: usize = parts
        .next()
        .unwrap_or("")
        .parse()
        .map_err(|_| GitError::Corrupt("bad object size".into()))?;
    Ok(Object { kind, data: inflated[nul + 1..].to_vec() })
}

/// Parses a tree payload: `<mode> <name>\0<20-byte oid>` repeated.
pub fn parse_tree(data: &[u8]) -> Result<Vec<TreeEntry>> {
    let mut entries = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let space = rest
            .iter()
            .position(|&b| b == b' ')
            .ok_or_else(|| GitError::Corrupt("tree entry missing mode terminator".into()))?;
        let mode = std::str::from_utf8(&rest[..space])
            .map_err(|_| GitError::Corrupt("tree mode not UTF-8".into()))?
            .to_string();
        let nul = rest[space..]
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| GitError::Corrupt("tree entry missing name terminator".into()))?
            + space;
        let name = std::str::from_utf8(&rest[space + 1..nul])
            .map_err(|_| GitError::Corrupt("tree entry name not UTF-8".into()))?
            .to_string();
        if rest.len() < nul + 21 {
            return Err(GitError::Corrupt("tree entry truncated oid".into()));
        }
        let mut oid = [0u8; 20];
        oid.copy_from_slice(&rest[nul + 1..nul + 21]);
        entries.push(TreeEntry { mode, name, oid: Oid(oid) });
        rest = &rest[nul + 21..];
    }
    Ok(entries)
}

/// Parses a commit payload. Continuation header lines (leading space, e.g.
/// wrapped `gpgsig` bodies) are skipped.
pub fn parse_commit(data: &[u8]) -> Result<Commit> {
    let text = std::str::from_utf8(data)
        .map_err(|_| GitError::Corrupt("commit payload not UTF-8".into()))?;
    let mut tree = None;
    let mut parents = Vec::new();
    let mut author = None;
    let mut committer = None;

    let mut lines = text.split('\n');
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
        if line.starts_with(' ') {
            continue; // continuation of a multi-line header (e.g. gpgsig)
        }
        if let Some(rest) = line.strip_prefix("tree ") {
            tree = Some(
                Oid::from_hex(rest)
                    .ok_or_else(|| GitError::Corrupt("bad commit tree oid".into()))?,
            );
        } else if let Some(rest) = line.strip_prefix("parent ") {
            parents.push(
                Oid::from_hex(rest)
                    .ok_or_else(|| GitError::Corrupt("bad commit parent oid".into()))?,
            );
        } else if let Some(rest) = line.strip_prefix("author ") {
            author = Some(Signature::parse(rest)?);
        } else if let Some(rest) = line.strip_prefix("committer ") {
            committer = Some(Signature::parse(rest)?);
        }
    }
    // The message is everything after the first blank line.
    let message = lines.collect::<Vec<&str>>().join("\n");

    let tree = tree.ok_or_else(|| GitError::Corrupt("commit missing tree header".into()))?;
    let author = author.ok_or_else(|| GitError::Corrupt("commit missing author".into()))?;
    let committer = committer.unwrap_or_else(|| author.clone());
    Ok(Commit { tree, parents, author, committer, message })
}

/// Renders a signature back to its on-disk form.
#[cfg(feature = "pro")]
pub fn render_signature(sig: &Signature) -> String {
    format!("{} <{}> {} {}", sig.name, sig.email, sig.time, sig.timezone)
}

/// Computes the blob oid for arbitrary bytes: git's object id is the SHA-1
/// of `"blob <len>\0" ++ bytes`. Used by status comparisons (free) and
/// object writing (pro).
pub fn blob_oid(bytes: &[u8]) -> Oid {
    let mut hasher = sha1_smol::Sha1::new();
    hasher.update(format!("blob {}\0", bytes.len()).as_bytes());
    hasher.update(bytes);
    let digest = hasher.digest().bytes();
    Oid(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commit_with_gpgsig() {
        let raw = b"\
tree 0123456789abcdef0123456789abcdef01234567
parent 89abcdef0123456789abcdef0123456789abcdef
author Ada Lovelace <ada@example.com> 1739000000 +1300
committer Grace Hopper <grace@example.com> 1739000100 +1300
gpgsig -----BEGIN PGP SIGNATURE-----\n iQEcBAABCgAGBQ\n =abcd\n 
Add docs/readme.md

First commit body
with a second line.
";
        let commit = parse_commit(raw).unwrap();
        assert_eq!(
            commit.tree,
            Oid::from_hex("0123456789abcdef0123456789abcdef01234567").unwrap()
        );
        assert_eq!(commit.parents.len(), 1);
        assert_eq!(commit.author.name, "Ada Lovelace");
        assert_eq!(commit.author.timezone, "+1300");
        assert_eq!(commit.message, "First commit body\nwith a second line.\n");
    }

    #[test]
    fn parses_tree_entries() {
        let mut data = Vec::new();
        data.extend_from_slice(b"100644 readme.md\0");
        data.extend_from_slice(&[0x01u8, 0x23]);
        data.extend_from_slice(&[0u8; 18]);
        data.extend_from_slice(b"40000 docs\0");
        data.extend_from_slice(&[0xffu8; 20]);
        let entries = parse_tree(&data).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "readme.md");
        assert!(!entries[0].is_dir());
        assert_eq!(entries[1].name, "docs");
        assert!(entries[1].is_dir());
    }
}
