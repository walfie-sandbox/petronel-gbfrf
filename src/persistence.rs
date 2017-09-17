use chrono::{DateTime, NaiveDateTime, Utc};
use petronel::error::*;
use petronel::model::{BossImageUrl, RaidBoss, RaidBossMetadata};
use prost::Message;
use protobuf;
use redis::{self, Commands};
use serde_json;
use std::collections::HashSet;

pub struct Cache {
    redis_connection: redis::Connection,
    bosses_key: String,
    legacy_bosses_key: Option<String>,
}

impl Cache {
    pub fn new<U>(url: U, bosses_key: String, legacy_bosses_key: Option<String>) -> Result<Self>
    where
        U: AsRef<str>,
    {
        let redis_connection = redis::Client::open(url.as_ref())
            .chain_err(|| "failed to start Redis client")?
            .get_connection()
            .chain_err(|| "failed to get Redis connection")?;

        Ok(Cache {
            redis_connection,
            bosses_key,
            legacy_bosses_key,
        })
    }

    pub fn save_bosses(&self, bosses: &[RaidBossMetadata]) -> Result<()> {
        let json = serde_json::to_vec(bosses).chain_err(
            || "failed to serialize boss data to cache",
        )?;

        self.redis_connection
            .set(&self.bosses_key, json)
            .chain_err(|| "failed to persist boss data to cache")
    }

    pub fn get_bosses(&self) -> Result<Vec<RaidBossMetadata>> {
        let bosses = self.get_petronel_bosses()?;
        if bosses.is_empty() {
            self.get_legacy_bosses()
        } else {
            Ok(bosses)
        }
    }

    fn get_petronel_bosses(&self) -> Result<Vec<RaidBossMetadata>> {
        let bytes: Option<Vec<u8>> = self.redis_connection.get(&self.bosses_key).chain_err(
            || "failed to load boss data from cache",
        )?;

        if bytes.is_none() {
            return Ok(Vec::new());
        }

        serde_json::from_slice::<Vec<RaidBossMetadata>>(bytes.unwrap().as_ref())
            .chain_err(|| "failed to parse boss data from cache")
    }

    fn get_legacy_bosses(&self) -> Result<Vec<RaidBossMetadata>> {
        let cache_key = match self.legacy_bosses_key {
            Some(ref key) => key,
            None => return Ok(Vec::new()),
        };

        let bytes: Option<Vec<u8>> = self.redis_connection.get(cache_key).chain_err(
            || "failed to load legacy boss data from cache",
        )?;

        if bytes.is_none() {
            return Ok(Vec::new());
        }

        let mut bosses_proto = protobuf::LegacyRaidBossesCacheItem::decode(bytes.unwrap())
            .chain_err(|| "failed to parse legacy boss data from cache")?
            .raid_bosses;

        let output = bosses_proto
            .drain(..)
            .map(|boss_proto| {
                let mut translations = HashSet::with_capacity(1);
                if let Some(translation) = boss_proto.translated_name {
                    translations.insert(translation.into());
                }

                let boss = RaidBoss {
                    name: boss_proto.name.into(),
                    level: boss_proto.level as i16,
                    image: boss_proto.image.map(BossImageUrl::from),
                    language: protobuf::convert::language_from_proto(boss_proto.language),
                    translations,
                };

                let last_seen = DateTime::<Utc>::from_utc(
                    NaiveDateTime::from_timestamp(boss_proto.last_seen, 0),
                    Utc,
                );

                RaidBossMetadata {
                    boss,
                    last_seen,
                    image_hash: None, // TODO
                }
            })
            .collect();

        Ok(output)
    }
}
