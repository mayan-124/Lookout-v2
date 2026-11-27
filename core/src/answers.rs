//! The answer line for date and time zone questions, and copying it.

use std::sync::Arc;

use chrono::{DateTime, Local, Offset};

use crate::actions::disk::TextClipboard;
use crate::actions::{Action, ActionError, Authorized, Executed, Plan, Preview, Risk, Target};
use crate::dates::{self, Reply};
use crate::item::{Item, Kind};
use crate::query::ParsedQuery;
use crate::sources::{QueryContext, Source, Tier, ANSWER_SCORE};
use crate::zones;

pub const COPY: &str = "answer_copy";
/// `Kind::Custom` names of answer results.
pub const KIND_DATE: &str = "date_answer";
pub const KIND_TIME: &str = "time_answer";
pub const KIND_INFO: &str = "answer_info";
pub const KIND_CALC: &str = "calculator";
pub const KIND_CONVERSION: &str = "conversion";

/// The date or time zone answer to `input`, with the result's source and kind.
fn read<Tz: chrono::TimeZone>(
    input: &str,
    now: &DateTime<Tz>,
) -> Option<(&'static str, &'static str, Reply)> {
    if let Some(r) = dates::answer(input, now.date_naive()) {
        return Some(("dates", KIND_DATE, r));
    }
    let here = now.offset().fix().local_minus_utc() / 60;
    zones::answer(input, now.to_utc(), here).map(|r| ("zones", KIND_TIME, r))
}

/// Whether `input` is a question this answers. Its words are then not read as
/// date filters or operators. A question without an answer is not: the words
/// may well be a file search. Asks Windows nothing: it runs on every keystroke.
pub fn is_question<Tz: chrono::TimeZone>(input: &str, now: &DateTime<Tz>) -> bool {
    matches!(
        dates::answer(input, now.date_naive()),
        Some(Reply::Answer { .. })
    ) || zones::is_known(input)
}

pub struct AnswerSource;

impl Source for AnswerSource {
    fn id(&self) -> &'static str {
        "answers"
    }

    fn tier(&self) -> Tier {
        Tier::Instant
    }

    fn query(&self, q: &ParsedQuery, _ctx: &QueryContext, emit: &mut dyn FnMut(Vec<Item>)) {
        if q.kind.is_some() {
            return;
        }
        let Some((source, kind, reply)) = read(&q.raw, &Local::now()) else {
            return;
        };
        let mut item = match reply {
            Reply::Answer { answer, detail } => {
                let mut item = Item::new(
                    format!("answer:{source}"),
                    Kind::Custom(kind.into()),
                    answer.clone(),
                    source,
                );
                item.subtitle = Some(detail);
                item.uri = Some(answer);
                item.metadata.insert("default_action".into(), COPY.into());
                item
            }
            Reply::Problem(why) => Item::new(
                format!("answer:{source}:none"),
                Kind::Custom(KIND_INFO.into()),
                why,
                source,
            ),
        };
        item.score = ANSWER_SCORE;
        item.score_features.match_quality = 1.0;
        emit(vec![item]);
    }
}

/// Puts an answer on the clipboard.
pub struct CopyAnswer(pub Arc<dyn TextClipboard>);

impl Action for CopyAnswer {
    fn id(&self) -> &str {
        COPY
    }
    fn label(&self) -> &str {
        "Copy"
    }
    fn applies_to(&self, kind: &Kind) -> bool {
        // A typed web address is copied the same way.
        const COPIED: [&str; 5] = [
            KIND_DATE,
            KIND_TIME,
            KIND_CALC,
            KIND_CONVERSION,
            crate::actions::quick::KIND_URL,
        ];
        matches!(kind, Kind::Custom(n) if COPIED.contains(&n.as_str()))
    }
    fn plan(&self, target: &Target) -> Result<Plan, ActionError> {
        let text = target
            .uri
            .clone()
            .ok_or_else(|| ActionError::Refused("there is nothing to copy".into()))?;
        Ok(Plan {
            risk: Risk::Safe,
            preview: Preview {
                from: None,
                title: "Copy".into(),
                lines: vec![text.clone()],
                warnings: vec![],
            },
            undoable: false,
            snapshot: serde_json::json!({ "text": text }),
            fingerprint: format!("{COPY}|{text}"),
        })
    }
    fn execute(&self, plan: &Plan, _: &Authorized) -> Result<Executed, ActionError> {
        let text = plan.snapshot["text"]
            .as_str()
            .ok_or_else(|| ActionError::Failed("there is nothing to copy".into()))?;
        self.0.set_text(text).map_err(ActionError::Failed)?;
        Ok(Executed {
            message: format!("Copied: {text}"),
            undo_token: None,
        })
    }
}
