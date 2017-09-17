use bytes::Bytes;
use petronel;
use petronel::model::Message as PetronelMessage;
use protobuf;
use std::time::{SystemTime, UNIX_EPOCH};
use websocket;

#[cfg_attr(rustfmt, rustfmt_skip)]
const DEFAULT_IMAGE: &'static str =
    "https://abs.twimg.com/sticky/default_profile_images/default_profile_mini.png";

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

fn boss_to_proto(boss: &petronel::model::RaidBoss) -> protobuf::RaidBoss {
    protobuf::RaidBoss {
        name: boss.name.to_string(),
        image: boss.image.clone().map(|i| i.to_string()),
        last_seen: now_as_milliseconds(),
        level: boss.level as i32,
        language: language_to_proto(boss.language),
        translated_name: boss.translations.iter().next().map(|t| t.to_string()),
    }
}

fn tweet_to_proto(tweet: &petronel::model::RaidTweet) -> protobuf::RaidTweetResponse {
    protobuf::RaidTweetResponse {
        boss_name: tweet.boss_name.to_string(),
        raid_id: tweet.raid_id.to_string(),
        screen_name: tweet.user.to_string(),
        tweet_id: tweet.tweet_id as i64,
        profile_image: tweet.user_image.clone().map(|i| i.to_string()).unwrap_or(
            DEFAULT_IMAGE.to_string(),
        ),
        text: tweet.text.clone().map(|t| t.to_string()).unwrap_or(
            "".to_string(),
        ),
        created_at: tweet.created_at.timestamp() * 1000,
        language: language_to_proto(tweet.language),
    }
}

pub(crate) fn petronel_message_to_bytes(msg: PetronelMessage) -> Option<Bytes> {
    use self::PetronelMessage::*;
    use protobuf::ResponseMessage;
    use protobuf::response_message::Data::*;

    let data = match msg {
        Heartbeat => Some(KeepAliveMessage(protobuf::KeepAliveResponse {})),
        Tweet(tweet) => Some(RaidTweetMessage(tweet_to_proto(tweet))),
        TweetList(tweets) => {
            // TODO: This is pretty inefficient
            let mut total_len = 0;
            let mut frames = Vec::with_capacity(tweets.len());

            for tweet in tweets {
                let message = RaidTweetMessage(tweet_to_proto(tweet));
                let response = ResponseMessage { data: Some(message) };
                if let Some(bytes) = websocket::serialize_protobuf(response) {
                    total_len += bytes.len();
                    frames.push(bytes);
                }
            }

            let mut bytes = Bytes::with_capacity(total_len);
            for frame in frames {
                bytes.extend(frame);
            }

            return Some(bytes);
        }
        BossUpdate(boss) => Some(RaidBossesMessage(protobuf::RaidBossesResponse {
            raid_bosses: vec![boss_to_proto(boss)],
        })),
        BossList(bosses) => Some(RaidBossesMessage(protobuf::RaidBossesResponse {
            raid_bosses: bosses.iter().cloned().map(boss_to_proto).collect(),
        })),
        BossRemove(_boss_name) => None,
    };

    data.and_then(|d| {
        websocket::serialize_protobuf(ResponseMessage { data: Some(d) })
    })
}
