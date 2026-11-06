"""Real smart-collection trigger dependency conformance (migration0037)."""
import pathlib, sqlite3, unittest
class Dependencies(unittest.TestCase):
 def test_topic_scope(self):
  for topic in ('catalog','metadata','libraries','sources','devices','profiles','viewing','organization','unrelated'):
   db=sqlite3.connect(':memory:')
   db.executescript('CREATE TABLE change_events(topic TEXT,resource_id TEXT); CREATE TABLE collections(kind TEXT,revision INTEGER,profile_id TEXT,filter_id TEXT); CREATE TABLE saved_filters(id TEXT); CREATE TABLE version_files(id TEXT);')
   db.executescript((pathlib.Path(__file__).resolve().parents[1]/'migrations/0037_smart_collection_revisions.sql').read_text())
   db.executemany('INSERT INTO collections VALUES (?,?,?,?)',[('smart',1,'p','f'),('smart',1,'q','f'),('manual',1,'p','f')])
   db.execute('INSERT INTO change_events VALUES (?,?)',(topic,'p'))
   rows=db.execute('SELECT revision FROM collections ORDER BY rowid').fetchall()
   global_change=topic in ('catalog','metadata','libraries','sources','devices','profiles')
   self.assertEqual(rows,[(1+int(global_change or topic=='viewing'),),(1+int(global_change),),(1,)])
   db.close()

class Preparations(unittest.TestCase):
 def test_file_dependency_changes_only(self):
  root=pathlib.Path(__file__).resolve().parents[1]
  for column,value,changed in [('revision','r',False),('revision','next',True),('available',1,False),('available',0,True),('library_id','lib',False),('library_id','other',True),('edition_id','ed',False),('edition_id','other',True),('title','next',False)]:
   with self.subTest(column=column,value=value):
    db=sqlite3.connect(':memory:')
    db.executescript('''CREATE TABLE media_files(id TEXT,revision TEXT,available INTEGER,library_id TEXT,edition_id TEXT,title TEXT);
      CREATE TABLE preparations(timeline_id TEXT,job_id TEXT,revision INTEGER);
      CREATE TABLE processing_jobs(id TEXT,source_file_id TEXT,output_file_id TEXT);
      CREATE TABLE libraries(id TEXT,enabled INTEGER);
      CREATE TABLE version_files(version_id TEXT);
      CREATE TABLE media_versions(id TEXT,timeline_id TEXT,origin TEXT);
      CREATE TABLE timelines(id TEXT,edition_id TEXT);
      CREATE TABLE renditions(source TEXT,external_id TEXT);
      CREATE TRIGGER preparation_catalog_revision AFTER INSERT ON media_files BEGIN SELECT 1; END;
      CREATE TRIGGER preparation_binding_insert AFTER INSERT ON version_files BEGIN SELECT 1; END;
      CREATE TRIGGER preparation_binding_update AFTER UPDATE ON version_files BEGIN SELECT 1; END;
      CREATE TRIGGER preparation_binding_delete AFTER DELETE ON version_files BEGIN SELECT 1; END;''')
    db.executescript((root/'migrations/0054_preparation_dependencies.sql').read_text())
    db.execute("INSERT INTO media_files VALUES ('f','r',1,'lib','ed','title')")
    db.execute("INSERT INTO processing_jobs VALUES ('j','f','out')")
    db.execute("INSERT INTO preparations VALUES ('t','j',1)")
    db.execute(f'UPDATE media_files SET {column}=? WHERE id=?',(value,'f'))
    self.assertEqual(db.execute('SELECT revision FROM preparations').fetchone()[0],1+changed)
    db.close()

class RelationshipDependencies(unittest.TestCase):
 def test_exact_affected_preparations(self):
  cases = [
   ("INSERT INTO version_files VALUES ('v1')", (1, 0)),
   ("UPDATE version_files SET version_id='v2' WHERE version_id='v1'", (1, 1)),
   ("DELETE FROM version_files WHERE version_id='v1'", (1, 0)),
   ("INSERT INTO media_versions VALUES ('new','t1','original')", (1, 0)),
   ("UPDATE media_versions SET timeline_id='t2' WHERE id='v1'", (1, 1)),
   ("UPDATE media_versions SET origin='generated' WHERE id='v1'", (1, 0)),
   ("DELETE FROM media_versions WHERE id='v1'", (1, 0)),
   ("UPDATE timelines SET edition_id='new' WHERE id='t1'", (1, 0)),
   ("UPDATE libraries SET enabled=0 WHERE id='lib'", (1, 0)),
   ("UPDATE libraries SET enabled=1 WHERE id='lib'", (0, 0)),
   ("INSERT INTO renditions VALUES ('playscale','j1')", (1, 0)),
   ("INSERT INTO renditions VALUES ('external','j1')", (0, 0)),
   ("UPDATE renditions SET external_id='j2' WHERE source='playscale'", (1, 1)),
   ("UPDATE renditions SET external_id='j2' WHERE source='external'", (0, 0)),
   ("DELETE FROM renditions WHERE source='playscale'", (1, 0)),
   ("DELETE FROM renditions WHERE source='external'", (0, 0)),
   ("UPDATE media_files SET revision='next' WHERE id='out'", (1, 0)),
  ]
  for sql, delta in cases:
   with self.subTest(sql=sql):
    db=sqlite3.connect(':memory:')
    db.executescript("""CREATE TABLE media_files(id TEXT,revision TEXT,available INTEGER,library_id TEXT,edition_id TEXT);
     CREATE TABLE preparations(timeline_id TEXT,job_id TEXT,revision INTEGER);
     CREATE TABLE processing_jobs(id TEXT,source_file_id TEXT,output_file_id TEXT);
     CREATE TABLE libraries(id TEXT,enabled INTEGER);
     CREATE TABLE version_files(version_id TEXT);
     CREATE TABLE media_versions(id TEXT,timeline_id TEXT,origin TEXT);
     CREATE TABLE timelines(id TEXT,edition_id TEXT);
     CREATE TABLE renditions(source TEXT,external_id TEXT);
     CREATE TRIGGER preparation_catalog_revision AFTER INSERT ON media_files BEGIN SELECT 1; END;
     CREATE TRIGGER preparation_binding_insert AFTER INSERT ON version_files BEGIN SELECT 1; END;
     CREATE TRIGGER preparation_binding_update AFTER UPDATE ON version_files BEGIN SELECT 1; END;
     CREATE TRIGGER preparation_binding_delete AFTER DELETE ON version_files BEGIN SELECT 1; END;
     INSERT INTO libraries VALUES ('lib',1),('other',1);
     INSERT INTO media_files VALUES ('f','r',1,'lib','ed'),('out','r',1,'other','ed'),('f2','r',1,'other','ed');
     INSERT INTO processing_jobs VALUES ('j1','f','out'),('j2','f2','out2');
     INSERT INTO timelines VALUES ('t1','ed'),('t2','ed');
     INSERT INTO media_versions VALUES ('v1','t1','original'),('v2','t2','original');
     INSERT INTO version_files VALUES ('v1');
     INSERT INTO renditions VALUES ('playscale','j1'),('external','j1');
     INSERT INTO preparations VALUES ('t1','j1',1),('t2','j2',1);""")
    db.executescript((pathlib.Path(__file__).resolve().parents[1]/'migrations/0054_preparation_dependencies.sql').read_text())
    db.execute(sql)
    self.assertEqual(db.execute('SELECT revision FROM preparations ORDER BY timeline_id').fetchall(),[(1+delta[0],),(1+delta[1],)])
    db.close()

if __name__=='__main__':unittest.main()

