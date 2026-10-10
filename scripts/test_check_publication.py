"""Git identity rules of the publication privacy check, against disposable repositories."""
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import check_publication  # noqa: E402

PROJECT = 'Motion contributors', 'contributors@motion.invalid'
PRIVATE = 'Someone', 'someone@private.invalid.example'
WEB_FLOW = 'GitHub', 'noreply@github.com'


def commit(repo, author, committer, message):
    env = dict(os.environ, GIT_AUTHOR_NAME=author[0], GIT_AUTHOR_EMAIL=author[1],
               GIT_COMMITTER_NAME=committer[0], GIT_COMMITTER_EMAIL=committer[1])
    subprocess.run(['git', 'commit', '-q', '--allow-empty', '-m', message], cwd=repo, env=env, check=True)
    return subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip()


class Identities(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.repo = pathlib.Path(self.directory.name)
        subprocess.run(['git', 'init', '-q', '-b', 'main'], cwd=self.repo, check=True)
        commit(self.repo, PROJECT, PROJECT, 'project')

    def tearDown(self):
        self.directory.cleanup()

    def test_project_noreply_and_web_flow_committer_pass(self):
        noreply = 'Contributor', '1+contributor@users.noreply.github.com'
        commit(self.repo, noreply, WEB_FLOW, 'web merge')
        self.assertEqual(check_publication.identity_failures(self.repo, set()), [])

    def test_web_flow_identity_is_not_an_author_identity(self):
        sha = commit(self.repo, WEB_FLOW, PROJECT, 'authored as GitHub')
        self.assertEqual(check_publication.identity_failures(self.repo, set()),
                         [f'Git history: commit {sha[:12]} author must use a public noreply or project-only identity'])

    def test_private_identity_fails_without_revealing_it(self):
        sha = commit(self.repo, PRIVATE, PRIVATE, 'private')
        failures = check_publication.identity_failures(self.repo, set())
        self.assertEqual([f[:33] for f in failures], [f'Git history: commit {sha[:12]} '] * 2)
        self.assertNotIn(PRIVATE[1], '\n'.join(failures))

    def test_exception_covers_only_its_commit_and_role(self):
        published = commit(self.repo, PRIVATE, WEB_FLOW, 'published web merge')
        later = commit(self.repo, PRIVATE, PROJECT, 'new private commit')
        failures = check_publication.identity_failures(self.repo, {(published, 'author'), (later, 'committer')})
        self.assertEqual(failures, [f'Git history: commit {later[:12]} author must use a public '
                                    'noreply or project-only identity'])

    def test_unrelated_branches_are_not_published_by_head(self):
        subprocess.run(['git', 'checkout', '-q', '-b', 'local-work'], cwd=self.repo, check=True)
        commit(self.repo, PRIVATE, PRIVATE, 'local only')
        subprocess.run(['git', 'checkout', '-q', 'main'], cwd=self.repo, check=True)
        self.assertEqual(check_publication.identity_failures(self.repo, set()), [])

    def test_exception_file_is_strict(self):
        path = self.repo / 'exceptions.txt'
        path.write_text('# comment\n' + 'a' * 40 + ' author  # reason\n')
        self.assertEqual(check_publication.history_exceptions(path), {('a' * 40, 'author')})
        for bad in ('abc author', 'a' * 40 + ' reviewer', 'A' * 40 + ' author', 'a' * 40):
            path.write_text(bad + '\n')
            with self.assertRaises(SystemExit):
                check_publication.history_exceptions(path)

    def test_checked_in_exceptions_parse(self):
        self.assertTrue(check_publication.history_exceptions())


if __name__ == '__main__':
    unittest.main()
