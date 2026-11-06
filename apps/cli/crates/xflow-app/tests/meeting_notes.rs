use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;
use xflow_app::meeting::{notes, MeetingStore};
use xflow_providers::llm::*;
// Varied, authored transcript conversations, not ASR-quality evidence.
const TRANSCRIPTS: &[&str] = &[
 "Maya: We can ship Kubernetes on Tuesday. Omar: Wait, PostgreSQL backup failed. Maya: Then postpone the release; Omar will test restore tomorrow. No budget was approved.",
 "अदिति: शुक्रवार को नई वेबसाइट जारी करें? रवि: भुगतान की जाँच बाकी है। अदिति: ठीक है, अभी तारीख तय नहीं करते। रवि कल OAuth की जाँच करेगा। खर्च की सीमा २५०० रुपये है।",
 "José: Propongo publicar el viernes. Lucía: Todavía falla el acceso OAuth. José: Entonces no fijemos fecha. Lucía revisará los permisos mañana; el presupuesto sigue pendiente.",
 "Ana: Vamos migrar o PostgreSQL na segunda? João: O backup ainda não foi validado. Ana: Adiamos a migração. João fará o teste de restauração amanhã. Não aprovamos despesas novas.",
 "田中: 金曜日に公開しましょうか。佐藤: まだ認証テストが終わっていません。田中: では公開日は未定です。佐藤が明日テストします。予算は承認していません。",
 "Neha: Kal deploy karte hain? Arjun: Nahi, useEffect loop aur OAuth bug pending hai. Neha: Okay, release hold karo. Arjun kal fix verify karega; deadline abhi confirm nahi hai. Budget 42,000 se upar mat jaana.",
];
struct Fake {
    calls: AtomicUsize,
    bad: bool,
    terse: bool,
}
#[async_trait]
impl ChatProvider for Fake {
    async fn warm(&self) -> Result<(), LlmError> {
        Ok(())
    }
    async fn stream(&self, r: &ChatRequest, tx: &mpsc::Sender<Event>) -> Result<(), LlmError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        assert!(r.tools.is_empty());
        let Message::User(text) = &r.messages[0] else {
            panic!()
        };
        assert!(text.len() < 16000);
        let start = text.find('\n').unwrap() + 1;
        let end = text.rfind('\n').unwrap();
        let segments: Vec<xflow_core::meeting::Segment> =
            serde_json::from_str(&text[start..end]).unwrap();
        let claims:Vec<_>=segments.iter().take(if self.terse {1} else {usize::MAX}).map(|s|serde_json::json!({"kind":"summary","text":if self.terse {"Cited excerpt"} else {&s.text},"segment_id":if self.bad {-1} else {s.id},"revision":s.revision,"quote":s.text})).collect();
        tx.send(Event::Text(serde_json::to_string(&claims).unwrap()))
            .await
            .unwrap();
        tx.send(Event::Done(Stop::End)).await.unwrap();
        Ok(())
    }
}
fn imported(store: &MeetingStore, count: usize) -> xflow_core::meeting::MeetingId {
    let mut v: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/meeting-notes/mixed-script.json")).unwrap();
    v["gaps"] = serde_json::json!([]);
    v["ended_at_ms"] = serde_json::json!(1700000000000i64 + 10_800_000);
    v["segments"]=serde_json::json!((0..count).map(|i|serde_json::json!({"source_id":"room","sequence":i,"range":{"start_ms":i*10546,"end_ms":i*10546+10546},"text":format!("{} [{i}]",TRANSCRIPTS[i%6])})).collect::<Vec<_>>());
    store
        .import_json(&serde_json::to_vec(&v).unwrap())
        .unwrap()
        .meeting
}
#[tokio::test]
async fn multilingual_citations_dictionary_revisions_and_delete() {
    let d = tempfile::tempdir().unwrap();
    let s = MeetingStore::at(d.path().join("meetings.db"));
    let id = imported(&s, 6);
    let f = Fake {
        calls: AtomicUsize::new(0),
        bad: false,
        terse: false,
    };
    let n = notes::generate(&s, &id, &f, "fake", 32000).await.unwrap();
    assert_eq!(n.claims.len(), 6);
    for (i, c) in n.claims.iter().enumerate() {
        assert!(c.quote.contains(TRANSCRIPTS[i]));
        assert_eq!(c.revision, 1);
    }
    assert!(s.reader().notes(&id).unwrap().is_some());
    let bad = Fake {
        calls: AtomicUsize::new(0),
        bad: true,
        terse: false,
    };
    assert!(notes::generate(&s, &id, &bad, "fake", 32000).await.is_err());
    assert_eq!(s.reader().notes(&id).unwrap().unwrap().claims.len(), 6);
    assert!(notes::generate(&s, &id, &f, "fake", 1000).await.is_err());
    s.set_term("PostgreSQL", "Postgres").unwrap();
    assert_eq!(s.apply_terms(&id).unwrap(), 2);
    assert!(s.reader().notes(&id).unwrap().is_none());
    s.delete(&id).unwrap();
    assert!(s.reader().notes(&id).is_err());
}
#[tokio::test]
async fn three_hours_page_sequentially_and_fail_closed_at_output_bound() {
    let d = tempfile::tempdir().unwrap();
    let s = MeetingStore::at(d.path().join("meetings.db"));
    let id = imported(&s, 1024);
    let f = Fake {
        calls: AtomicUsize::new(0),
        bad: false,
        terse: false,
    };
    // The fake quotes every sentence, deliberately exceeding the bounded note budget.
    let result = notes::generate(&s, &id, &f, "fake", 32000).await;
    assert!(result.unwrap_err().to_string().contains("128 KiB"));
    assert!(f.calls.load(Ordering::Relaxed) > 10);
    assert!(s.reader().notes(&id).unwrap().is_none());
    let terse = Fake {
        calls: AtomicUsize::new(0),
        bad: false,
        terse: true,
    };
    let notes = notes::generate(&s, &id, &terse, "fake", 32000)
        .await
        .unwrap();
    assert!(!notes.claims.is_empty());
    assert!(terse.calls.load(Ordering::Relaxed) > 10);
    assert_eq!(s.reader().status(&id).unwrap().summary.segment_count, 1024);
}

#[tokio::test]
async fn real_public_transcript_excerpts_preserve_all_six_languages() {
    let rows: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/meeting-notes/public-transcripts.json"
    ))
    .unwrap();
    let rows = rows.as_array().unwrap();
    let d = tempfile::tempdir().unwrap();
    let store = MeetingStore::at(d.path().join("meetings.db"));
    let mut v: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/meeting-notes/mixed-script.json")).unwrap();
    v["gaps"] = serde_json::json!([]);
    v["segments"]=serde_json::json!(rows.iter().enumerate().map(|(i,r)|serde_json::json!({"source_id":"room","sequence":i,"range":{"start_ms":i*1000,"end_ms":i*1000+1000},"text":r["text"]})).collect::<Vec<_>>());
    let id = store
        .import_json(&serde_json::to_vec(&v).unwrap())
        .unwrap()
        .meeting;
    let fake = Fake {
        calls: AtomicUsize::new(0),
        bad: false,
        terse: false,
    };
    let notes = notes::generate(&store, &id, &fake, "fake", 32000)
        .await
        .unwrap();
    assert_eq!(notes.claims.len(), 6);
    for (claim, row) in notes.claims.iter().zip(rows) {
        assert_eq!(
            claim.quote,
            row["text"].as_str().unwrap(),
            "{}",
            row["language"]
        );
        assert_eq!(claim.revision, 1);
    }
}
