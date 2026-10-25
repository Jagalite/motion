//! Fixed borrowed-input admission for the retained archive capture only.
//! Logical accounting, not peak RSS: six simultaneous node representations
//! cover archive, control/checkpoint decode, typed snapshot, fingerprint Value,
//! canonical encoding and reread comparison. Raw24 covers retained strings,
//! escaped parser scratch and those encoder/clone buffers. Two node/eight raw
//! shares remain reserved with the witness; the other four/sixteen are scratch.
use anyhow::{Context, ensure};
use rusqlite::{Connection, types::ValueRef};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
const NODE: u64 = 1024;
pub(super) struct Budget<'a> {
    retaining: bool,
    remaining: u64,
    used: u64,
    retained: u64,
    deadline: Instant,
    cancel: &'a AtomicBool,
}
impl<'a> Budget<'a> {
    pub(super) fn new(
        remaining: u64,
        retained: u64,
        deadline: Instant,
        cancel: &'a AtomicBool,
    ) -> anyhow::Result<Self> {
        ensure!(
            remaining <= 256 * 1024 * 1024 && retained <= remaining,
            "archive aggregate allocation exceeded"
        );
        let b = Self {
            retaining: true,
            remaining,
            used: retained,
            retained: 0,
            deadline,
            cancel,
        };
        b.alive()?;
        Ok(b)
    }
    pub(super) fn alive(&self) -> anyhow::Result<()> {
        ensure!(
            !self.cancel.load(Ordering::Acquire) && Instant::now() < self.deadline,
            "archive capture cancelled or deadline expired"
        );
        Ok(())
    }
    fn add(&mut self, n: u64, retained: u64) -> anyhow::Result<()> {
        self.alive()?;
        self.used = self
            .used
            .checked_add(n)
            .context("archive allocation overflow")?;
        ensure!(
            self.used <= self.remaining,
            "archive aggregate allocation exceeded"
        );
        self.retained = self
            .retained
            .checked_add(if self.retaining { retained } else { 0 })
            .context("archive retention overflow")?;
        Ok(())
    }
    pub(super) fn retained(&self) -> u64 {
        self.retained
    }
    fn raw(&mut self, raw: &[u8], json: bool) -> anyhow::Result<()> {
        let len = u64::try_from(raw.len())?;
        // Admit escape scratch BEFORE running even the allocation-free visitor.
        self.add(
            len.checked_mul(24).context("archive raw overflow")?,
            len.checked_mul(8).context("archive raw overflow")?,
        )?;
        if json {
            let mut d = serde_json::Deserializer::from_slice(raw);
            Count(self).deserialize(&mut d)?;
            d.end()?;
        }
        Ok(())
    }
    pub(super) fn scan(
        &mut self,
        db: &Connection,
        sql: &str,
        json: &[usize],
        retaining: bool,
    ) -> anyhow::Result<()> {
        self.alive()?;
        self.retaining = retaining;
        let mut statement = db.prepare(sql)?;
        let columns = statement.column_count();
        let mut rows = statement.query([])?;
        let mut count = 0usize;
        while let Some(row) = rows.next()? {
            count = count.checked_add(1).context("archive rows overflow")?;
            ensure!(count <= 16384, "archive row cohort exceeded");
            self.add(6 * NODE, 2 * NODE)?;
            for i in 0..columns {
                match row.get_ref(i)? {
                    ValueRef::Text(raw) => self.raw(raw, json.contains(&i))?,
                    ValueRef::Integer(_) | ValueRef::Null => self.add(48, 16)?,
                    _ => anyhow::bail!("archive invalid scalar storage class"),
                }
            }
        }
        self.alive()
    }
}
struct Count<'a, 'b>(&'a mut Budget<'b>);
impl<'de> DeserializeSeed<'de> for Count<'_, '_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        self.0
            .add(6 * NODE, 2 * NODE)
            .map_err(serde::de::Error::custom)?;
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Count<'_, '_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bounded archived JSON")
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<(), A::Error> {
        while a.next_element_seed(Count(self.0))?.is_some() {}
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<(), A::Error> {
        while a.next_key_seed(Count(self.0))?.is_some() {
            a.next_value_seed(Count(self.0))?;
        }
        Ok(())
    }
}
