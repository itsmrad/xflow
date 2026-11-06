use super::store::{check_meeting, highwater, open_reader, store_id, ContractError};
use anyhow::{ensure, Result};
use rusqlite::{params, Connection, Row};
use serde::Serialize;
use std::path::PathBuf;
use xflow_core::meeting::*;

const SEGMENT_COLUMNS: &str =
    "s.meeting_id,s.id,s.source_id,s.sequence,s.start_ms,s.end_ms,s.revision,s.text";
const SUMMARY_COLUMNS: &str = "m.id,m.title,m.started_at_ms,m.ended_at_ms,m.timezone,m.edit_revision,(SELECT count(*) FROM segments WHERE meeting_id=m.id),(SELECT count(*) FROM gaps WHERE meeting_id=m.id),m.import_id LIKE 'audio:%'";

#[derive(Clone, Debug)]
pub struct MeetingReader {
    path: PathBuf,
}
impl MeetingReader {
    /// No I/O; every operation opens READ_ONLY|NOFOLLOW and sets query_only=ON.
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }
    fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let mut db = open_reader(&self.path)?;
        // A short snapshot makes validation + page atomic. Never retained between calls.
        let tx = db.transaction()?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn status(&self, meeting: &MeetingId) -> Result<MeetingStatus> {
        self.read(|db| {
            check_meeting(db, meeting)?;
            let summary = summary(db, meeting)?;
            let mut sources =
                db.prepare("SELECT json FROM sources WHERE meeting_id=?1 ORDER BY id")?;
            let sources = sources
                .query_map([meeting.id], |r| r.get::<_, String>(0))?
                .map(|s| Ok(serde_json::from_str(&s?)?))
                .collect::<Result<Vec<Source>>>()?;
            let mut gaps = db.prepare("SELECT json FROM gaps WHERE meeting_id=?1 ORDER BY id")?;
            let gaps = gaps
                .query_map([meeting.id], |r| r.get::<_, String>(0))?
                .map(|s| Ok(serde_json::from_str(&s?)?))
                .collect::<Result<Vec<Gap>>>()?;
            // Global revision, same clock as list/search, so a delete invalidates export.
            let snapshot = Snapshot {
                highwater: highwater(db, "segments")?,
                edit_revision: global_revision(db)?,
            };
            let result = MeetingStatus {
                summary,
                sources,
                gaps,
                snapshot,
            };
            ensure!(
                fits(&result)?,
                ContractError::InvalidQuery("status exceeds page budget")
            );
            Ok(result)
        })
    }

    /// Half-open overlap, keyset order (start_ms,id). Also the paged export seam.
    pub fn transcript(&self, query: &TranscriptQuery) -> Result<TranscriptPage> {
        valid_range(query.range)?;
        self.read(|db| {
            match check_meeting(db, &query.meeting) {
                Ok(()) => {}
                Err(error)
                    if error.downcast_ref::<ContractError>() == Some(&ContractError::NotFound)
                        && (query.cursor.is_some() || query.snapshot.is_some()) =>
                {
                    return Err(ContractError::StaleCursor.into());
                }
                Err(error) => return Err(error),
            }
            // Corrections and deletes bump the global revision; a new import does not.
            let revision = global_revision(db)?;
            let mut frozen=query.snapshot;
            let (mut last_start,mut last_id)=(-1,0);
            if let Some(c)=&query.cursor {
                ensure!(c.meeting==query.meeting && c.range==query.range && frozen.is_none_or(|s|s==c.snapshot) && c.last_start_ms>=0 && c.last_id>0, ContractError::StaleCursor);
                frozen=Some(c.snapshot); last_start=c.last_start_ms; last_id=c.last_id;
            }
            let snapshot=snapshot(db,"segments",revision,frozen)?;
            let mut statement=db.prepare(&format!("SELECT {SEGMENT_COLUMNS} FROM segments s WHERE s.meeting_id=?1 AND s.start_ms<?2 AND s.end_ms>?3 AND s.id<=?4 AND (s.start_ms,s.id)>(?5,?6) ORDER BY s.start_ms,s.id LIMIT 21"))?;
            let rows=statement.query_map(params![query.meeting.id,query.range.end_ms,query.range.start_ms,snapshot.highwater,last_start,last_id], |r|segment(r,&query.meeting.store_id))?;
            let mut page=TranscriptPage {snapshot,segments:Vec::new(),next:None};
            for row in rows {
                let item=row?;
                if page.segments.len()==MAX_PAGE_SEGMENTS { break; }
                let next=TranscriptCursor {meeting:query.meeting.clone(),range:query.range,snapshot,last_start_ms:item.range.start_ms,last_id:item.id};
                page.segments.push(item);
                let old_next=page.next.replace(next);
                if !fits(&page)? {
                    page.segments.pop(); page.next=old_next;
                    ensure!(!page.segments.is_empty(), ContractError::InvalidQuery("segment exceeds page budget"));
                    return Ok(page);
                }
            }
            // Exactly-full pages may have an empty terminal page; no COUNT/offset scan.
            if page.segments.len()<MAX_PAGE_SEGMENTS { page.next=None; }
            Ok(page)
        })
    }

    pub fn list(&self, filter: &ListFilter, cursor: Option<&ListCursor>) -> Result<MeetingPage> {
        self.list_limited(filter, cursor, MAX_PAGE_SEGMENTS)
    }

    pub fn list_limited(
        &self,
        filter: &ListFilter,
        cursor: Option<&ListCursor>,
        limit: usize,
    ) -> Result<MeetingPage> {
        ensure!(
            (1..=MAX_PAGE_SEGMENTS).contains(&limit),
            ContractError::InvalidQuery("invalid limit")
        );
        ensure!(
            filter.since_ms >= 0 && filter.until_ms > filter.since_ms,
            ContractError::InvalidQuery("invalid start-date interval")
        );
        self.read(|db| {
            let identity=store_id(db)?;
            let revision=global_revision(db)?;
            let mut frozen=None;
            let mut last=0;
            if let Some(c)=cursor {
                ensure!(c.store_id==identity && c.filter==*filter && c.last_id>0, ContractError::StaleCursor);
                frozen=Some(c.snapshot); last=c.last_id;
            }
            let snap=snapshot(db,"meetings",revision,frozen)?;
            let mut statement=db.prepare(&format!("SELECT {SUMMARY_COLUMNS} FROM meetings m WHERE m.id>?1 AND m.id<=?2 AND m.started_at_ms>=?3 AND m.started_at_ms<?4 ORDER BY m.id LIMIT 21"))?;
            let rows=statement.query_map(params![last,snap.highwater,filter.since_ms,filter.until_ms], |r| summary_row(r,&identity))?;
            let mut page=MeetingPage {meetings:Vec::new(),next:None};
            for row in rows {
                if page.meetings.len()==limit {break;}
                let item=row?;
                let next=ListCursor {store_id:identity.clone(),filter:filter.clone(),snapshot:snap,last_id:item.meeting.id};
                page.meetings.push(item);
                let old=page.next.replace(next);
                if !fits(&page)? {
                    page.meetings.pop(); page.next=old;
                    ensure!(!page.meetings.is_empty(), ContractError::InvalidQuery("summary exceeds page budget"));
                    return Ok(page);
                }
            }
            if page.meetings.len()<limit {page.next=None;}
            Ok(page)
        })
    }

    /// The read-only connection must reject writes.
    #[doc(hidden)]
    pub fn write_refused(&self) -> Result<()> {
        let db = open_reader(&self.path)?;
        match db.execute("UPDATE metadata SET cleanup_pending=cleanup_pending", []) {
            Err(error) => {
                let message = error.to_string().to_ascii_lowercase();
                ensure!(
                    message.contains("readonly"),
                    "reader write failed without a readonly error: {error}"
                );
                Ok(())
            }
            Ok(_) => anyhow::bail!("read-only meeting reader wrote to the database"),
        }
    }

    /// Literal, case-sensitive Unicode substring search; never accepts FTS syntax.
    /// Short (<3 scalar) terms inspect at most 256 indexed candidates per call.
    pub fn search(&self, query: &str, cursor: Option<&SearchCursor>) -> Result<SearchPage> {
        self.search_limited(query, cursor, MAX_PAGE_SEGMENTS)
    }

    pub fn search_limited(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        limit: usize,
    ) -> Result<SearchPage> {
        ensure!(
            (1..=MAX_PAGE_SEGMENTS).contains(&limit),
            ContractError::InvalidQuery("invalid limit")
        );
        ensure!(
            !query.is_empty()
                && query.len() <= 256
                && query.chars().count() <= 128
                && !query.contains('\0'),
            ContractError::InvalidQuery("search requires 1..128 scalars / <=256 bytes without NUL")
        );
        self.read(|db| {
            let identity=store_id(db)?;
            let revision=global_revision(db)?;
            let (frozen,mut last)=if let Some(c)=cursor {
                ensure!(c.store_id==identity && c.query==query && c.last_id>0, ContractError::StaleCursor);
                (Some(c.snapshot),c.last_id)
            } else {(None,0)};
            let snap=snapshot(db,"segments",revision,frozen)?;
            let short=query.chars().count()<3;
            let quoted=format!("\"{}\"",query.replace('"',"\"\""));
            let sql=if short {
                format!("SELECT {SEGMENT_COLUMNS} FROM segments s WHERE s.id>?1 AND s.id<=?2 ORDER BY s.id LIMIT 256")
            } else {
                format!("SELECT {SEGMENT_COLUMNS} FROM segment_fts JOIN segments s ON s.id=segment_fts.rowid WHERE segment_fts MATCH ?3 AND segment_fts.rowid>?1 AND segment_fts.rowid<=?2 ORDER BY segment_fts.rowid LIMIT 256")
            };
            let mut statement=db.prepare(&sql)?;
            let mut rows=if short { statement.query(params![last,snap.highwater])? } else {statement.query(params![last,snap.highwater,quoted])?};
            let mut page=SearchPage {segments:Vec::new(),next:None};
            let mut scanned=0;
            let mut exhausted=true;
            while let Some(row)=rows.next()? {
                let item=segment(row,&identity)?;
                scanned+=1;
                if item.text.contains(query) {
                    if page.segments.len()==limit {exhausted=false;break;}
                    let id=item.id;
                    page.segments.push(item);
                    let old=page.next.replace(SearchCursor {store_id:identity.clone(),query:query.to_owned(),snapshot:snap,last_id:id});
                    if !fits(&page)? {
                        page.segments.pop();page.next=old;exhausted=false;break;
                    }
                }
                last=row.get(1)?;
            }
            if scanned==256 {exhausted=false;}
            if exhausted {page.next=None;} else {
                ensure!(last>0, ContractError::InvalidQuery("search item exceeds page budget"));
                page.next=Some(SearchCursor {store_id:identity,query:query.to_owned(),snapshot:snap,last_id:last});
            }
            Ok(page)
        })
    }
}
fn global_revision(db: &Connection) -> Result<i64> {
    Ok(db.query_row("SELECT edit_revision FROM metadata", [], |r| r.get(0))?)
}
fn snapshot(
    db: &Connection,
    table: &str,
    revision: i64,
    frozen: Option<Snapshot>,
) -> Result<Snapshot> {
    let highwater = highwater(db, table)?;
    let snap = frozen.unwrap_or(Snapshot {
        highwater,
        edit_revision: revision,
    });
    ensure!(
        snap.highwater >= 0 && snap.highwater <= highwater && snap.edit_revision == revision,
        ContractError::StaleCursor
    );
    Ok(snap)
}
fn valid_range(range: TimeRange) -> Result<()> {
    ensure!(
        range.start_ms >= 0
            && range.end_ms > range.start_ms
            && range.end_ms - range.start_ms <= MAX_WINDOW_MS,
        ContractError::InvalidQuery("range must be nonnegative, nonempty and <=10 minutes")
    );
    Ok(())
}
fn fits(value: &impl Serialize) -> Result<bool> {
    Ok(serde_json::to_vec(value)?.len() <= PAGE_BYTES)
}
fn segment(row: &Row<'_>, store: &str) -> rusqlite::Result<Segment> {
    let id = row.get(1)?;
    Ok(Segment {
        meeting: MeetingId {
            store_id: store.into(),
            id: row.get(0)?,
        },
        id,
        source_id: row.get(2)?,
        sequence: row.get(3)?,
        range: TimeRange {
            start_ms: row.get(4)?,
            end_ms: row.get(5)?,
        },
        ingest_sequence: id,
        revision: row.get(6)?,
        text: row.get(7)?,
    })
}
fn summary_row(row: &Row<'_>, store: &str) -> rusqlite::Result<MeetingSummary> {
    Ok(MeetingSummary {
        meeting: MeetingId {
            store_id: store.into(),
            id: row.get(0)?,
        },
        title: row.get(1)?,
        started_at_ms: row.get(2)?,
        ended_at_ms: row.get(3)?,
        timezone: row.get(4)?,
        edit_revision: row.get(5)?,
        segment_count: row.get(6)?,
        gap_count: row.get(7)?,
        provenance: if row.get(8)? {
            Provenance::TranscribedAudio
        } else {
            Provenance::ImportedUnverified
        },
        notes: NotesStatus::Absent,
    })
}
fn summary(db: &Connection, meeting: &MeetingId) -> Result<MeetingSummary> {
    Ok(db.query_row(
        &format!("SELECT {SUMMARY_COLUMNS} FROM meetings m WHERE m.id=?1"),
        [meeting.id],
        |r| summary_row(r, &meeting.store_id),
    )?)
}
