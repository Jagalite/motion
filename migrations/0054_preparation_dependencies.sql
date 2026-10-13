-- A05: representation revisions depend on inputs, not every catalog event.
DROP TRIGGER preparation_catalog_revision;
DROP TRIGGER preparation_binding_insert;
DROP TRIGGER preparation_binding_update;
DROP TRIGGER preparation_binding_delete;
CREATE INDEX preparation_timeline ON preparations(timeline_id);
CREATE INDEX processing_source_file ON processing_jobs(source_file_id);
CREATE INDEX processing_output_file ON processing_jobs(output_file_id);

CREATE TRIGGER preparation_file_update AFTER UPDATE OF revision,available,library_id,edition_id ON media_files
WHEN OLD.revision IS NOT NEW.revision OR OLD.available IS NOT NEW.available OR OLD.library_id IS NOT NEW.library_id OR OLD.edition_id IS NOT NEW.edition_id BEGIN
 UPDATE preparations SET revision=revision+1 WHERE job_id IN (SELECT id FROM processing_jobs WHERE source_file_id=NEW.id OR output_file_id=NEW.id);
END;
CREATE TRIGGER preparation_source_enabled AFTER UPDATE OF enabled ON libraries WHEN OLD.enabled IS NOT NEW.enabled BEGIN
 UPDATE preparations SET revision=revision+1 WHERE job_id IN (SELECT j.id FROM media_files f JOIN processing_jobs j ON j.source_file_id=f.id WHERE f.library_id=NEW.id);
END;
CREATE TRIGGER preparation_binding_insert AFTER INSERT ON version_files BEGIN
 UPDATE preparations SET revision=revision+1 WHERE timeline_id IN (SELECT timeline_id FROM media_versions WHERE id=NEW.version_id);
END;
CREATE TRIGGER preparation_binding_update AFTER UPDATE ON version_files BEGIN
 UPDATE preparations SET revision=revision+1 WHERE timeline_id IN (SELECT timeline_id FROM media_versions WHERE id IN (OLD.version_id,NEW.version_id));
END;
CREATE TRIGGER preparation_binding_delete BEFORE DELETE ON version_files BEGIN
 UPDATE preparations SET revision=revision+1 WHERE timeline_id IN (SELECT timeline_id FROM media_versions WHERE id=OLD.version_id);
END;
CREATE TRIGGER preparation_version_insert AFTER INSERT ON media_versions BEGIN
 UPDATE preparations SET revision=revision+1 WHERE timeline_id=NEW.timeline_id;
END;
CREATE TRIGGER preparation_version_update AFTER UPDATE OF timeline_id,origin ON media_versions BEGIN
 UPDATE preparations SET revision=revision+1 WHERE timeline_id IN (OLD.timeline_id,NEW.timeline_id);
END;
-- Capture the parent before a cascading binding deletion loses that lookup.
CREATE TRIGGER preparation_version_delete BEFORE DELETE ON media_versions BEGIN
 UPDATE preparations SET revision=revision+1 WHERE timeline_id=OLD.timeline_id;
END;
CREATE TRIGGER preparation_timeline_edition AFTER UPDATE OF edition_id ON timelines BEGIN
 UPDATE preparations SET revision=revision+1 WHERE timeline_id=NEW.id;
END;
CREATE TRIGGER preparation_rendition_insert AFTER INSERT ON renditions WHEN NEW.source='playscale' BEGIN
 UPDATE preparations SET revision=revision+1 WHERE job_id=NEW.external_id;
END;
CREATE TRIGGER preparation_rendition_update AFTER UPDATE ON renditions BEGIN
 UPDATE preparations SET revision=revision+1 WHERE job_id IN (SELECT OLD.external_id WHERE OLD.source='playscale' UNION SELECT NEW.external_id WHERE NEW.source='playscale');
END;
CREATE TRIGGER preparation_rendition_delete AFTER DELETE ON renditions WHEN OLD.source='playscale' BEGIN
 UPDATE preparations SET revision=revision+1 WHERE job_id=OLD.external_id;
END;
