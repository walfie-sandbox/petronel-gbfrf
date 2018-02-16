use chrono::{DateTime, NaiveDateTime, Utc};
use futures::{self, Async, Future, Stream};
use futures::sync::{mpsc, oneshot};
use futures_cpupool::{CpuFuture, CpuPool};
use petronel::error::*;
use petronel::model::{BossImageUrl, RaidBoss, RaidBossMetadata};
use prost::Message;
use protobuf;
use redis::{self, Commands};
use serde_json;
use std::collections::HashSet;

enum CacheMessage {
    Get(oneshot::Sender<Result<Vec<RaidBossMetadata>>>),
    Update(Vec<RaidBossMetadata>),
}

pub struct AsyncCacheClient(mpsc::UnboundedSender<CacheMessage>);
impl AsyncCacheClient {
    pub fn get_bosses(&self) -> Box<Future<Item = Vec<RaidBossMetadata>, Error = Error>> {
        let (tx, rx) = oneshot::channel();
        let _ = self.0.unbounded_send(CacheMessage::Get(tx));

        Box::new(rx.map_err(|_| "failed to get cached bosses").flatten())
    }

    pub fn update_bosses(&self, bosses: Vec<RaidBossMetadata>) {
        let _ = self.0.unbounded_send(CacheMessage::Update(bosses));
    }
}

pub struct AsyncCache {
    cache: Cache,
    receiver: mpsc::UnboundedReceiver<CacheMessage>,
}

impl AsyncCache {
    pub fn no_op(pool: &CpuPool) -> (AsyncCacheClient, CpuFuture<(), Error>) {
        let (sender, receiver) = mpsc::unbounded();

        let cpu_future = pool.spawn(NoOpCache(receiver));

        (AsyncCacheClient(sender), cpu_future)
    }

    pub fn new(
        pool: &CpuPool,
        url: String,
        bosses_key: String,
        legacy_bosses_key: Option<String>,
    ) -> (AsyncCacheClient, CpuFuture<(), Error>) {
        let (sender, receiver) = mpsc::unbounded();

        let lazy = futures::future::lazy(move || -> Result<AsyncCache> {
            let redis_connection = redis::Client::open(url.as_ref())
                .chain_err(|| "failed to create Redis client")?
                .get_connection()
                .chain_err(|| "failed to get Redis connection")?;

            let cache = Cache {
                redis_connection,
                bosses_key,
                legacy_bosses_key,
            };

            Ok(AsyncCache { cache, receiver })
        });

        let cpu_future = pool.spawn(lazy.flatten());

        (AsyncCacheClient(sender), cpu_future)
    }
}

impl Future for AsyncCache {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> ::std::result::Result<Async<Self::Item>, Self::Error> {
        use self::CacheMessage::*;
        loop {
            let polled = self.receiver
                .poll()
                .map_err(|()| "failed to poll cache requests stream");
            match try_ready!(polled) {
                Some(Get(sender)) => {
                    let _ = sender.send(self.cache.get_bosses());
                }
                Some(Update(bosses)) => {
                    if let Err(e) = self.cache.save_bosses(&bosses) {
                        eprintln!("failed to save to cache: {:?}", e)
                    }
                }
                None => {
                    return Ok(Async::Ready(()));
                }
            }
        }
    }
}

struct NoOpCache(mpsc::UnboundedReceiver<CacheMessage>);
impl Future for NoOpCache {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> ::std::result::Result<Async<Self::Item>, Self::Error> {
        loop {
            let polled = self.0
                .poll()
                .map_err(|()| "failed to poll cache requests stream");
            let _ = try_ready!(polled);
        }
    }
}

struct Cache {
    redis_connection: redis::Connection,
    bosses_key: String,
    legacy_bosses_key: Option<String>,
}

impl Cache {
    pub fn save_bosses(&self, bosses: &[RaidBossMetadata]) -> Result<()> {
        let json =
            serde_json::to_vec(bosses).chain_err(|| "failed to serialize boss data to cache")?;

        self.redis_connection
            .set(&self.bosses_key, json)
            .chain_err(|| "failed to persist boss data to cache")
    }

    pub fn get_bosses(&self) -> Result<Vec<RaidBossMetadata>> {
        let bosses = self.get_petronel_bosses()?;

        if bosses.is_empty() {
            let legacy_bosses = self.get_legacy_bosses()?;
            if !legacy_bosses.is_empty() {
                eprintln!(
                    "Legacy gbf-raidfinder boss cache found. Importing bosses to new format."
                );
                self.save_bosses(&legacy_bosses)?;
            }

            Ok(legacy_bosses)
        } else {
            Ok(bosses)
        }
    }

    fn get_petronel_bosses(&self) -> Result<Vec<RaidBossMetadata>> {
        let bytes: Option<Vec<u8>> = self.redis_connection
            .get(&self.bosses_key)
            .chain_err(|| "failed to load boss data from cache")?;

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

        let bytes: Option<Vec<u8>> = self.redis_connection
            .get(cache_key)
            .chain_err(|| "failed to load legacy boss data from cache")?;

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
                    NaiveDateTime::from_timestamp(boss_proto.last_seen / 1000, 0),
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
