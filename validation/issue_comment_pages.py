"""Read Forgejo issue comments through its paginated repository endpoint."""

from datetime import datetime, timedelta, timezone
import re

from validation import python_unit_receipts as receipts


def pages(api, path, query=None):
    number = int(path.split('/')[2])
    issue = api.get(f'/issues/{number}')
    browser = api.root.rsplit('/api/v1/repos/', 1)[0] + '/' + api.repository
    targets = {api.root + f'/{kind}/{number}' for kind in ('issues', 'pulls')}
    html_targets = {browser + f'/{kind}/{number}' for kind in ('issues', 'pulls')}
    receipts.require(receipts.positive(issue.get('number')) == number and issue.get('url') in targets
                     and issue.get('html_url') in html_targets,
                     'Issue comment scope metadata mismatch')
    receipts.require(issue['url'].rsplit('/', 2)[-2] == issue['html_url'].rsplit('/', 2)[-2],
                     'Issue comment API and browser kind differ')
    created = datetime.fromisoformat(issue['created_at'])
    receipts.require(created.tzinfo is not None and created.utcoffset() is not None,
                     'Issue comment creation time has no timezone')
    earliest = (created.astimezone(timezone.utc) - timedelta(seconds=1)).isoformat()
    receipts.require(query is None or set(query) <= {'since', 'before'},
                     'Unknown issue comment filter')
    filters = {'since': earliest, **(query or {})}
    scope = {issue['url'], issue['html_url']}
    prefix = '(?:' + '|'.join(re.escape(value) for value in (api.root, browser)) + ')'
    links = re.compile(prefix + r'/(?:issues|pulls)/[1-9][0-9]*')
    seen, selected = set(), []
    for page in range(1, receipts.MAX_PAGES + 1):
        batch = api.get('/issues/comments', {**filters, 'page': page, 'limit': 50})
        receipts.require(isinstance(batch, list) and len(batch) <= 50,
                         'Invalid repository comment pagination')
        for comment in batch:
            receipts.require(isinstance(comment, dict), 'Invalid repository comment')
            identity = receipts.positive(comment['id'])
            receipts.require(identity not in seen, 'Repeated repository comment identity')
            seen.add(identity)
            locations = [comment.get(key) for key in ('issue_url', 'pull_request_url')
                         if comment.get(key)]
            receipts.require(len(locations) == 1 and isinstance(locations[0], str)
                             and links.fullmatch(locations[0]),
                             'Repository comment scope URL mismatch')
            if locations[0] in scope:
                selected.append(comment)
        # Pagination is decided before filtering, using the whole repository batch.
        if len(batch) < 50:
            return selected
    raise receipts.ReceiptError('Evidence pagination limit reached')
