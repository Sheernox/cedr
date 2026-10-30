//! Minimal protobuf wire reader for Antigravity's usage metadata, independent of message content.
use crate::{
    Result,
    config::Config,
    store::{Row, Store},
};
use std::{collections::BTreeMap, path::Path};
#[derive(Debug)]
enum Field<'a> {
    Number(u64),
    Bytes(&'a [u8]),
}
fn varint(b: &[u8], i: &mut usize) -> Result<u64> {
    let mut n = 0;
    for shift in (0..70).step_by(7) {
        let x = *b.get(*i).ok_or("short protobuf varint")?;
        *i += 1;
        if shift == 63 && x > 1 {
            return Err("protobuf overflow".into());
        }
        n |= u64::from(x & 127) << shift;
        if x & 128 == 0 {
            return Ok(n);
        }
    }
    Err("protobuf overflow".into())
}
fn fields(b: &[u8]) -> Result<BTreeMap<u64, Field<'_>>> {
    let mut out = BTreeMap::new();
    let mut i = 0;
    while i < b.len() {
        let tag = varint(b, &mut i)?;
        let field = match tag & 7 {
            0 => Field::Number(varint(b, &mut i)?),
            2 => {
                let n = usize::try_from(varint(b, &mut i)?)?;
                let end = i
                    .checked_add(n)
                    .filter(|e| *e <= b.len())
                    .ok_or("short protobuf bytes")?;
                let value = &b[i..end];
                i = end;
                Field::Bytes(value)
            }
            1 | 5 => {
                i += if tag & 7 == 1 { 8 } else { 4 };
                if i > b.len() {
                    return Err("short protobuf fixed field".into());
                }
                continue;
            }
            _ => return Err("unsupported protobuf wire type".into()),
        };
        out.insert(tag >> 3, field);
    }
    Ok(out)
}
fn bytes<'a>(m: &BTreeMap<u64, Field<'a>>, k: u64) -> Result<&'a [u8]> {
    match m.get(&k) {
        Some(Field::Bytes(b)) => Ok(b),
        _ => Err("missing protobuf field".into()),
    }
}
fn num(m: &BTreeMap<u64, Field<'_>>, k: u64) -> Option<i64> {
    match m.get(&k) {
        Some(Field::Number(n)) => i64::try_from(*n).ok(),
        _ => None,
    }
}
fn timestamp(m: &BTreeMap<u64, Field<'_>>, k: u64) -> Option<i64> {
    let fields = fields(bytes(m, k).ok()?).ok()?;
    num(&fields, 1)?
        .checked_mul(1000)?
        .checked_add(num(&fields, 2).unwrap_or(0) / 1_000_000)
}
pub fn collect(path: &Path, c: &Config, db: &Store) -> Result<u64> {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.execute_batch("BEGIN")?;
    let generations: i64 = conn.query_row("SELECT count(*) FROM gen_metadata", [], |r| r.get(0))?;
    let steps: i64 = conn.query_row(
        "SELECT count(*) FROM steps WHERE step_type=15 AND metadata IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    if generations != steps {
        return Err("mid-write".into());
    }
    let cwd = std::fs::read(path.with_extension("meta"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v["cwd"].as_str().map(String::from))
        .unwrap_or_default();
    let Some(project) = c.project(&cwd) else {
        return Ok(0);
    };
    let mut g = conn.prepare("SELECT idx,data FROM gen_metadata ORDER BY idx")?;
    let mut s = conn.prepare(
        "SELECT metadata FROM steps WHERE step_type=15 AND metadata IS NOT NULL ORDER BY idx",
    )?;
    let mut gs = g.query([])?;
    let mut ss = s.query([])?;
    let mut malformed = 0;
    while let (Some(g), Some(s)) = (gs.next()?, ss.next()?) {
        let idx: i64 = g.get(0)?;
        let data: Vec<u8> = g.get(1)?;
        let step: Vec<u8> = s.get(0)?;
        let decoded = (|| -> Result<Row> {
            let outer = fields(&data)?;
            let one = fields(bytes(&outer, 1)?)?;
            let usage = fields(bytes(&one, 4)?)?;
            let md = fields(&step)?;
            let at = timestamp(&md, 1).ok_or("missing generation timestamp")?;
            let native = path.file_stem().unwrap_or_default().to_string_lossy();
            let r = Row {
                harness: "antigravity".into(),
                id: format!("{native}:{idx}"),
                session: format!(
                    "s-{}",
                    &crate::digest(&format!("antigravity:{native}"))[..12]
                ),
                at: at / 1000,
                project: project.clone(),
                model: super::bounded(std::str::from_utf8(bytes(&one, 19)?)?),
                input: num(&usage, 2),
                output: super::add(num(&usage, 3), num(&usage, 10)),
                cache_read: num(&usage, 5),
                reasoning: num(&usage, 10),
                context: super::add(num(&usage, 2), num(&usage, 5)),
                wall_ms: timestamp(&md, 7).filter(|t| *t >= at).map(|end| end - at),
                ..Row::default()
            };
            if !super::valid(&r) {
                return Err("invalid generation counters".into());
            }
            Ok(r)
        })();
        match decoded {
            Ok(r) => db.upsert(&r)?,
            Err(_) => malformed += 1,
        }
    }
    Ok(malformed)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn pb_bytes(tag: u8, b: &[u8]) -> Vec<u8> {
        let mut out = if tag < 16 {
            vec![tag << 3 | 2]
        } else {
            vec![(tag << 3 | 2) | 128, 1]
        };
        out.push(b.len() as u8);
        out.extend(b);
        out
    }
    fn pb_number(tag: u8, mut n: u64) -> Vec<u8> {
        let mut out = vec![tag << 3];
        while n > 127 {
            out.push((n as u8 & 127) | 128);
            n >>= 7;
        }
        out.push(n as u8);
        out
    }
    #[test]
    fn generation_and_mid_write() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("conversation.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE gen_metadata(idx INTEGER,data BLOB); CREATE TABLE steps(idx INTEGER,step_type INTEGER,metadata BLOB);").unwrap();
        let usage = [
            pb_number(2, 100),
            pb_number(3, 20),
            pb_number(5, 50),
            pb_number(10, 5),
        ]
        .concat();
        let one = [pb_bytes(4, &usage), pb_bytes(19, b"gemini-4-argon")].concat();
        conn.execute("INSERT INTO gen_metadata VALUES(0,?)", [pb_bytes(1, &one)])
            .unwrap();
        let db = Store::open(&d.path().join("monitor.sqlite")).unwrap();
        assert!(
            collect(&path, &Config::default(), &db)
                .unwrap_err()
                .to_string()
                .contains("mid-write")
        );
        let metadata = pb_bytes(1, &pb_number(1, 1793354400));
        conn.execute("INSERT INTO steps VALUES(0,15,?)", [metadata])
            .unwrap();
        collect(&path, &Config::default(), &db).unwrap();
        collect(&path, &Config::default(), &db).unwrap();
        let counts: (i64, i64, i64) = db
            .db
            .query_row(
                "SELECT count(*),sum(input),sum(output) FROM responses",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(counts, (1, 100, 25));
    }
    #[test]
    fn decode_and_reject_truncated() {
        let m = fields(&[8, 150, 1, 18, 2, 1, 2]).unwrap();
        assert_eq!(num(&m, 1), Some(150));
        assert_eq!(bytes(&m, 2).unwrap(), [1, 2]);
        assert!(fields(&[18, 3, 1]).is_err());
    }
}
