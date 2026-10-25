-- Durable exposed work tokens are not a disposable projection. Only a complete
-- retained native owner capture plus fixed library proof can issue a successor.
CREATE TABLE catabolic_aggregate_issuance (
 work_id TEXT PRIMARY KEY,
 state_json TEXT NOT NULL CHECK(json_valid(state_json))
 CHECK(json_extract(state_json,'$.contract_version') IS 1)
 CHECK(json_extract(state_json,'$.work_id') IS work_id)
 CHECK(json_type(state_json,'$.catalog_id') IS 'text')
 CHECK(json_type(state_json,'$.incarnation') IS 'text')
 CHECK(json_type(state_json,'$.motion_baseline') IS 'integer')
 CHECK(json_type(state_json,'$.revision') IS 'integer')
 CHECK(json_type(state_json,'$.observation_epoch') IS 'integer')
 CHECK(json_type(state_json,'$.native_generation') IS 'integer')
 CHECK(json_type(state_json,'$.dependency_digest') IS 'text')
 CHECK(json_extract(state_json,'$.motion_baseline')>0)
 CHECK(json_extract(state_json,'$.revision')>=json_extract(state_json,'$.motion_baseline'))
 CHECK(json_extract(state_json,'$.observation_epoch')>0)
);
CREATE TRIGGER catabolic_aggregate_issuance_identity BEFORE UPDATE ON catabolic_aggregate_issuance
 WHEN NEW.work_id IS NOT OLD.work_id
 OR json_extract(NEW.state_json,'$.catalog_id') IS NOT json_extract(OLD.state_json,'$.catalog_id')
 OR json_extract(NEW.state_json,'$.incarnation') IS NOT json_extract(OLD.state_json,'$.incarnation')
 OR json_extract(NEW.state_json,'$.motion_baseline') IS NOT json_extract(OLD.state_json,'$.motion_baseline')
 OR json_extract(NEW.state_json,'$.observation_epoch')<=json_extract(OLD.state_json,'$.observation_epoch')
 OR json_extract(NEW.state_json,'$.revision')<json_extract(OLD.state_json,'$.revision')
 BEGIN SELECT RAISE(ABORT,'aggregate issuance identity/order is immutable'); END;
CREATE TRIGGER catabolic_aggregate_issuance_retained BEFORE DELETE ON catabolic_aggregate_issuance
 BEGIN SELECT RAISE(ABORT,'aggregate issuance is retained'); END;
