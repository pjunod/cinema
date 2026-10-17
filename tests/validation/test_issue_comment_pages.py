"""Forgejo's per-issue comment API does not paginate; its repository API does."""

import unittest

from validation import python_unit_receipts as receipts


class CommentPagesCase(unittest.TestCase):
    def api(self, batches, metadata=None):
        api = receipts.API('https://forge.lan/api/v1', 'operator/plurx', 'fixture')
        api.calls = []
        issue = {'number': 17, 'url': api.root + '/pulls/17',
                 'html_url': 'https://forge.lan/operator/plurx/pulls/17',
                 'created_at': '2026-10-14T06:00:00Z'}

        def get(path, query=None):
            api.calls.append((path, query))
            if path == '/issues/17':
                return issue if metadata is None else metadata
            return batches[query['page'] - 1]

        api.get = get
        return api

    def comment(self, identity, number=17):
        return {'id': identity, 'body': 'fixture body', 'user': {'id': 1},
                'issue_url': '',
                'pull_request_url': f'https://forge.lan/operator/plurx/pulls/{number}'}

    def test_pages_whole_repository_before_filtering_target_issue(self):
        foreign = [self.comment(index, 18) for index in range(1, 51)]
        target = self.comment(51)
        api = self.api([foreign, [target]])
        self.assertEqual(api.pages('/issues/17/comments'), [target])
        self.assertEqual([path for path, _ in api.calls],
                         ['/issues/17', '/issues/comments', '/issues/comments'])
        self.assertEqual(api.calls[1][1],
                         {'since': '2026-10-14T05:59:59+00:00', 'page': 1, 'limit': 50})

    def test_scope_metadata_and_comment_urls_refuse_foreign_or_ambiguous_identity(self):
        for metadata in ({'number': 18}, {'number': 17, 'url': 'https://other.invalid'},
                         {'number': 17, 'url': 'https://forge.lan/api/v1/repos/operator/plurx/pulls/17',
                          'html_url': 'https://forge.lan/operator/plurx/pulls/17',
                          'created_at': '2026-10-14T06:00:00'}):
            with self.assertRaises(receipts.ReceiptError):
                self.api([[]], metadata).pages('/issues/17/comments')
        for location in ('https://other.invalid/pulls/17',
                         'https://forge.lan/operator/plurx/pulls/17#fragment',
                         'https://forge.lan/operator/plurx/pulls/017'):
            comment = self.comment(1)
            comment['pull_request_url'] = location
            with self.assertRaises(receipts.ReceiptError):
                self.api([[comment]]).pages('/issues/17/comments')
        comment = self.comment(1)
        comment['issue_url'] = 'https://forge.lan/operator/plurx/issues/17'
        with self.assertRaises(receipts.ReceiptError):
            self.api([[comment]]).pages('/issues/17/comments')

    def test_duplicate_pages_oversized_batches_and_page_budget_refuse(self):
        batch = [self.comment(index) for index in range(1, 51)]
        with self.assertRaises(receipts.ReceiptError):
            self.api([batch, batch]).pages('/issues/17/comments')
        with self.assertRaises(receipts.ReceiptError):
            self.api([batch + [self.comment(51)]]).pages('/issues/17/comments')
        pages = [[self.comment(page * 50 + index, 18) for index in range(1, 51)]
                 for page in range(receipts.MAX_PAGES)]
        with self.assertRaises(receipts.ReceiptError):
            self.api(pages).pages('/issues/17/comments')

    def test_empty_issues_filters_and_api_scoped_comment_urls_remain_exact(self):
        api = self.api([[]])
        self.assertEqual(api.pages('/issues/17/comments', {'before': '2026-10-15T00:00:00Z'}), [])
        self.assertEqual(api.calls[-1][1]['before'], '2026-10-15T00:00:00Z')
        for query in ({'limit': 500}, {'page': 2}):
            with self.assertRaises(receipts.ReceiptError):
                self.api([[]]).pages('/issues/17/comments', query)
        comment = self.comment(1)
        comment['pull_request_url'] = 'https://forge.lan/api/v1/repos/operator/plurx/pulls/17'
        self.assertEqual(self.api([[comment]]).pages('/issues/17/comments'), [comment])
        with self.assertRaises(receipts.ReceiptError):
            self.api([[]]).pages('/issues/17/comments', field='comments')

    def test_generic_pagination_and_nonforge_comment_paths_are_unchanged(self):
        api = self.api([[1] * 50, [2]])
        self.assertEqual(api.pages('/actions/runs'), [1] * 50 + [2])
        self.assertEqual(api.calls[0], ('/actions/runs', {'page': 1, 'limit': 50}))
        api = self.api([[3]])
        api.root = 'https://api.example.invalid/repos/operator/plurx'
        self.assertEqual(api.pages('/issues/17/comments'), [3])
        self.assertEqual(api.calls, [('/issues/17/comments', {'page': 1, 'limit': 50})])
