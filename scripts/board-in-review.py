#!/usr/bin/env python3
"""Puts the issues that commits pushed to dev name (#N) into review on the project
board: on the board, in the current iteration and "In review" (an issue already
"Done" stays so). Pull requests named (merges) are skipped.

    board-in-review.py <before> <after> [--dry-run]

Run by .github/workflows/board.yml on every push to dev. Needs GH_TOKEN: a classic
token with the `project` scope (a user's board can't be changed with the workflow's
own token), and GITHUB_REPOSITORY; the board is BOARD_OWNER's number BOARD_NUMBER.
--dry-run only says what it would change.
"""

import datetime
import json
import os
import re
import sys
import urllib.request

API = "https://api.github.com"
STATUS = "Status"
IN_REVIEW = "In review"
DONE = "Done"
ITERATION = "Iteration"


def issue_refs(messages):
    """The numbers referenced as `#N` in commit messages, in order, each once; not
    `owner/repo#N` (another repository).

    >>> issue_refs(["results: copy\\n\\nCloses #44", "part of #32 and #44", "x/y#9", "#7."])
    [44, 32, 7]
    """
    seen = []
    for message in messages:
        for match in re.finditer(r"(?<![\w/])#(\d+)\b", message):
            number = int(match.group(1))
            if number not in seen:
                seen.append(number)
    return seen


def current_iteration(iterations, today):
    """The iteration `today` falls in: started on or before it, not yet over.

    >>> its = [{"id": "a", "startDate": "2026-10-22", "duration": 14},
    ...        {"id": "b", "startDate": "2026-11-05", "duration": 14}]
    >>> [current_iteration(its, datetime.date(2026, m, d))
    ...  for m, d in [(10, 22), (11, 4), (11, 5), (10, 21), (12, 1)]]
    ['a', 'a', 'b', None, None]
    """
    for it in iterations:
        start = datetime.date.fromisoformat(it["startDate"])
        if start <= today < start + datetime.timedelta(days=it["duration"]):
            return it["id"]
    return None


def request(method, path, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(API + path, data=data, method=method)
    req.add_header("Authorization", f"Bearer {os.environ['GH_TOKEN']}")
    req.add_header("Accept", "application/vnd.github+json")
    with urllib.request.urlopen(req) as answer:
        return json.load(answer)


def graphql(query, **variables):
    answer = request("POST", "/graphql", {"query": query, "variables": variables})
    if answer.get("errors"):
        sys.exit(f"GitHub: {answer['errors']}")
    return answer["data"]


def messages(repo, before, after):
    """The messages of the commits the push brought."""
    if set(before) == {"0"}:
        return [request("GET", f"/repos/{repo}/commits/{after}")["commit"]["message"]]
    compared = request("GET", f"/repos/{repo}/compare/{before}...{after}")
    return [c["commit"]["message"] for c in compared["commits"]]


BOARD = """
query($owner: String!, $number: Int!) {
  user(login: $owner) { projectV2(number: $number) {
    id
    fields(first: 50) { nodes {
      ... on ProjectV2SingleSelectField { id name options { id name } }
      ... on ProjectV2IterationField { id name
        configuration { iterations { id title startDate duration } } }
    } }
  } }
}"""

ISSUE = """
query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) { issueOrPullRequest(number: $number) {
    __typename
    ... on Issue { id title projectItems(first: 20) { nodes {
      id project { id }
      status: fieldValueByName(name: "Status") {
        ... on ProjectV2ItemFieldSingleSelectValue { name } }
      iteration: fieldValueByName(name: "Iteration") {
        ... on ProjectV2ItemFieldIterationValue { iterationId } }
    } } }
  } }
}"""

ADD = """
mutation($project: ID!, $content: ID!) {
  addProjectV2ItemById(input: {projectId: $project, contentId: $content}) { item { id } }
}"""

SET = """
mutation($project: ID!, $item: ID!, $field: ID!, $value: ProjectV2FieldValue!) {
  updateProjectV2ItemFieldValue(input: {
    projectId: $project, itemId: $item, fieldId: $field, value: $value
  }) { projectV2Item { id } }
}"""


def main(before, after, dry_run):
    repo = os.environ["GITHUB_REPOSITORY"]
    owner, name = repo.split("/")
    board_owner = os.environ.get("BOARD_OWNER", owner)
    board_number = int(os.environ.get("BOARD_NUMBER", "1"))
    refs = issue_refs(messages(repo, before, after))
    if not refs:
        print("no issues named")
        return

    board = graphql(BOARD, owner=board_owner, number=board_number)["user"]["projectV2"]
    fields = {f["name"]: f for f in board["fields"]["nodes"] if f}
    status = fields[STATUS]
    in_review = next(o["id"] for o in status["options"] if o["name"] == IN_REVIEW)
    iteration = fields[ITERATION]
    current = current_iteration(
        iteration["configuration"]["iterations"], datetime.date.today()
    )

    def change(item, field, value, what):
        print(f"  {what}")
        if not dry_run:
            graphql(SET, project=board["id"], item=item, field=field, value=value)

    for number in refs:
        found = graphql(ISSUE, owner=owner, name=name, number=number)["repository"]
        issue = found["issueOrPullRequest"]
        if not issue or issue["__typename"] != "Issue":
            continue
        print(f"#{number} {issue['title']}")
        item = next(
            (i for i in issue["projectItems"]["nodes"] if i["project"]["id"] == board["id"]),
            None,
        )
        if item is None:
            print("  onto the board")
            item = {"id": None, "status": None, "iteration": None}
            if not dry_run:
                added = graphql(ADD, project=board["id"], content=issue["id"])
                item["id"] = added["addProjectV2ItemById"]["item"]["id"]
        state = (item.get("status") or {}).get("name")
        sprint = (item.get("iteration") or {}).get("iterationId")
        if current and sprint != current:
            change(item["id"], iteration["id"], {"iterationId": current}, "into the current iteration")
        if state not in (IN_REVIEW, DONE):
            change(item["id"], status["id"], {"singleSelectOptionId": in_review}, f"{state or 'no status'} → {IN_REVIEW}")


if __name__ == "__main__":
    args = [a for a in sys.argv[1:] if a != "--dry-run"]
    if len(args) != 2:
        sys.exit(__doc__)
    main(*args, dry_run="--dry-run" in sys.argv)
