//! Observable retained capacities supplement the input-node convention. This
//! does not claim allocator/RSS precision or access to private envelope buffers.
use super::*;
struct Count(u64);
impl Count {
    fn add(&mut self, n: usize) -> anyhow::Result<()> {
        self.0 = self
            .0
            .checked_add(u64::try_from(n)?)
            .context("archive capacity overflow")?;
        ensure!(self.0 <= 256 * 1024 * 1024, "archive capacity exceeded");
        Ok(())
    }
    fn text(&mut self, s: &String) -> anyhow::Result<()> {
        self.add(s.capacity())
    }
    fn vec<T>(&mut self, v: &Vec<T>) -> anyhow::Result<()> {
        self.add(
            v.capacity()
                .checked_mul(std::mem::size_of::<T>())
                .context("archive vector capacity overflow")?,
        )
    }
    fn value(&mut self, v: &Value) -> anyhow::Result<()> {
        self.add(1024)?;
        match v {
            Value::String(s) => self.text(s)?,
            Value::Array(a) => {
                self.vec(a)?;
                for v in a {
                    self.value(v)?;
                }
            }
            Value::Object(o) => {
                for (k, v) in o {
                    self.text(k)?;
                    self.value(v)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn checkpoint(&mut self, cp: &Checkpoint) -> anyhow::Result<()> {
        for s in [
            &cp.catalog_id,
            &cp.incarnation,
            &cp.import_id,
            &cp.input_digest,
            &cp.mapping_digest,
            &cp.reference_ack_id,
        ] {
            self.text(s)?;
        }
        Ok(())
    }
}
pub(super) fn observed(owner: &RetainedImportedArchive) -> anyhow::Result<u64> {
    let a = &owner.archive;
    let mut c = Count(std::mem::size_of::<RetainedImportedArchive>() as u64);
    c.text(&a.root)?;
    c.text(&a.identity.catalog_id)?;
    c.text(&a.identity.incarnation)?;
    if let Some((a, b, d, e)) = &a.control.single {
        for s in [a, b, d, e] {
            c.text(s)?;
        }
    }
    if let Some((a, b, d, _, e)) = &a.control.set {
        for s in [a, b, d, e] {
            c.text(s)?;
        }
    }
    c.vec(&a.control.parts)?;
    for (a, _, b, d, e) in &a.control.parts {
        for s in [a, b, d, e] {
            c.text(s)?;
        }
    }
    c.vec(&a.checkpoints)?;
    for (cp, raw) in &a.checkpoints {
        c.checkpoint(cp)?;
        c.text(raw)?;
    }
    c.vec(&a.entries)?;
    for e in &a.entries {
        c.checkpoint(&e.checkpoint)?;
        c.text(&e.audit_raw)?;
        let m = &e.snapshot.manifest;
        for s in [&m.import_id, &m.source_namespace, &m.source_snapshot_id] {
            c.text(s)?;
        }
        c.vec(&m.batches)?;
        for d in &m.batches {
            c.text(&d.sha256)?;
        }
        c.vec(&e.snapshot.batches)?;
        for b in &e.snapshot.batches {
            c.vec(&b.items)?;
            c.vec(&b.unresolved_evidence)?;
            for i in &b.items {
                for s in [&i.source_id, &i.item_id, &i.kind] {
                    c.text(s)?;
                }
                c.value(&i.legacy_metadata)?;
            }
            for u in &b.unresolved_evidence {
                c.text(&u.evidence_id)?;
                c.value(&u.data)?;
            }
            if let Some(t) = &b.typed {
                c.vec(&t.metadata)?;
                c.vec(&t.pins)?;
                c.vec(&t.tags)?;
                c.vec(&t.relationships)?;
                c.vec(&t.logical_aliases)?;
                for m in &t.metadata {
                    c.text(&m.item_id)?;
                    c.vec(&m.system_fields)?;
                    for s in &m.system_fields {
                        c.text(s)?;
                    }
                    c.value(&m.policies)?;
                    c.vec(&m.contributions)?;
                    for s in &m.contributions {
                        c.text(&s.namespace)?;
                        c.text(&s.record_id)?;
                        c.value(&s.fields)?;
                        c.value(&s.evidence)?;
                    }
                }
                // Nonmetadata typed descendants are freshly deserialized and
                // immutable; their modeled nodes remain in the retained reserve.
            }
        }
    }
    c.vec(&owner.activations)?;
    for command in &owner.activations {
        c.value(command.payload())?;
        // Private canonical/key String capacities are not exposed. Their fixed
        // freshly-constructed representation remains covered by raw8, not a
        // claim that this borrowed length is an observable capacity.
        c.add(command.canonical_payload().len())?;
    }
    c.vec(&owner.bridges)?;
    for b in &owner.bridges {
        for s in [
            &b.import_id,
            &b.item_id,
            &b.catalog_id,
            &b.incarnation,
            &b.proof_raw,
        ] {
            c.text(s)?;
        }
    }
    Ok(c.0)
}
