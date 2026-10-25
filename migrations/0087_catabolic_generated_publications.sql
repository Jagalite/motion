-- Private generated output ownership. Neither table grants public source access
-- or authorizes effects from request JSON; retained owners verify every proof.
CREATE TABLE catabolic_generated_source (
 id TEXT PRIMARY KEY CHECK(id='motion:generated-output'),
 mapping_json TEXT NOT NULL CHECK(json_valid(mapping_json)),
 created_at INTEGER NOT NULL
);
CREATE TRIGGER catabolic_generated_source_immutable BEFORE UPDATE ON catabolic_generated_source
 BEGIN SELECT RAISE(ABORT,'generated source identity is immutable'); END;
CREATE TRIGGER catabolic_generated_source_retained BEFORE DELETE ON catabolic_generated_source
 BEGIN SELECT RAISE(ABORT,'generated source identity is retained'); END;

-- Plan the final locator before copying bytes; retain uncertain attempts. The
-- exact measured output and command may be filled once, never rewritten on retry.
CREATE TABLE catabolic_generated_publications (
 operation_id TEXT PRIMARY KEY,
 job_id TEXT NOT NULL,
 attempt INTEGER NOT NULL CHECK(attempt>0),
 source_json TEXT NOT NULL CHECK(json_valid(source_json)),
 output_relative TEXT NOT NULL UNIQUE,
 output_json TEXT CHECK(output_json IS NULL OR json_valid(output_json)),
 command_json TEXT CHECK(command_json IS NULL OR json_valid(command_json)),
 receipt_json TEXT CHECK(receipt_json IS NULL OR json_valid(receipt_json)),
 acknowledged INTEGER NOT NULL DEFAULT 0 CHECK(acknowledged IN(0,1)),
 created_at INTEGER NOT NULL,
 UNIQUE(job_id,attempt),
 CHECK(command_json IS NULL OR output_json IS NOT NULL),
 CHECK(receipt_json IS NULL OR command_json IS NOT NULL),
 CHECK(acknowledged=0 OR receipt_json IS NOT NULL)
);
CREATE TRIGGER catabolic_generated_publication_identity BEFORE UPDATE ON catabolic_generated_publications
 WHEN NEW.operation_id IS NOT OLD.operation_id OR NEW.job_id IS NOT OLD.job_id
 OR NEW.attempt IS NOT OLD.attempt OR NEW.source_json IS NOT OLD.source_json
 OR NEW.output_relative IS NOT OLD.output_relative OR NEW.created_at IS NOT OLD.created_at
 BEGIN SELECT RAISE(ABORT,'generated publication identity is immutable'); END;
CREATE TRIGGER catabolic_generated_publication_proof BEFORE UPDATE ON catabolic_generated_publications
 WHEN (OLD.output_json IS NOT NULL AND NEW.output_json IS NOT OLD.output_json)
 OR (OLD.command_json IS NOT NULL AND NEW.command_json IS NOT OLD.command_json)
 OR (OLD.receipt_json IS NOT NULL AND NEW.receipt_json IS NOT OLD.receipt_json)
 OR NEW.acknowledged<OLD.acknowledged
 BEGIN SELECT RAISE(ABORT,'generated publication proof is immutable'); END;
CREATE TRIGGER catabolic_generated_publication_retained BEFORE DELETE ON catabolic_generated_publications
 BEGIN SELECT RAISE(ABORT,'generated publication intents are retained'); END;

-- Immutable native owners selected when the Motion preparation is admitted.
-- Later completion cannot substitute a newer timeline/version or occurrence.
CREATE TABLE catabolic_processing_admissions (
 job_id TEXT PRIMARY KEY,
 capture_json TEXT NOT NULL CHECK(json_valid(capture_json)),
 created_at INTEGER NOT NULL
);
CREATE TRIGGER catabolic_processing_admission_immutable BEFORE UPDATE ON catabolic_processing_admissions
 BEGIN SELECT RAISE(ABORT,'native processing admission is immutable'); END;
CREATE TRIGGER catabolic_processing_admission_retained BEFORE DELETE ON catabolic_processing_admissions
 BEGIN SELECT RAISE(ABORT,'native processing admission is retained'); END;
