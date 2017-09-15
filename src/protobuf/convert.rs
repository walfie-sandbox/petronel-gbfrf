use super::*;
use bytes::Bytes;
use petronel;
use petronel::model::Message as PetronelMessage;
use protobuf;
use std::time::{SystemTime, UNIX_EPOCH};
use websocket;

fn now_as_milliseconds() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => (duration.as_secs() * 1000) as i64,
        _ => 0,
    }
}

fn language_to_proto(language: petronel::model::Language) -> i32 {
    use petronel::model::Language::*;

    (match language {
         English => protobuf::Language::English,
         Japanese => protobuf::Language::Japanese,
         Other => protobuf::Language::Unspecified,
     }) as i32
}

pub(crate) fn petronel_message_to_bytes(msg: PetronelMessage) -> Option<Bytes> {
    use self::PetronelMessage::*;
    use protobuf::ResponseMessage;
    use protobuf::response_message::Data::*;

    let data = match msg {
        Heartbeat => Some(KeepAliveMessage(protobuf::KeepAliveResponse {})),
        Tweet(tweet) => None,
        TweetList(tweets) => None,
        BossUpdate(boss) => Some(RaidBossesMessage(protobuf::RaidBossesResponse {
            raid_bosses: vec![
                protobuf::RaidBoss {
                    name: boss.name.to_string(),
                    image: boss.image.clone().map(|i| i.to_string()),
                    last_seen: now_as_milliseconds(),
                    level: boss.level as i32,
                    language: language_to_proto(boss.language),
                    translated_name: boss.translations.iter().next().map(|t| t.to_string()),
                },
            ],
        })),
        BossList(bosses) => None,
        BossRemove(boss_name) => None,
    };

    data.and_then(|d| {
        websocket::serialize_protobuf(ResponseMessage { data: Some(d) })
    })
}
