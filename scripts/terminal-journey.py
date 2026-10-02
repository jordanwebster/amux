#!/usr/bin/env python3
"""Run a shared journey story on the real terminal client.

`scripts/terminal-journey.py <story>`: one story from journeys/manifest.json
against the served topology it names, with frames compared to reviewed
goldens (UPDATE_JOURNEY_GOLDENS=1 rewrites them) and independent
observations from the hosts. Results land in target/journeys/<story>.
"""

from __future__ import annotations

import hashlib
import os
from pathlib import Path
import re
import subprocess
import sys
import time

from journeys.terminal import ROOT, TerminalJourney, agent_row, at_home, at_rest, chat_of, story, working

PROMPT = "Check the deployment once."
REPLY = "The deploy check passed."
DRAFT = "Keep this draft through the outage."


def prompts(chat: dict, text: str) -> list[dict]:
    return [item for item in chat["items"] if item["text"] == text]


def reflected_once(chat: dict, text: str) -> None:
    """The prompt the person sent is in the host's chat exactly once."""
    found = prompts(chat, text)
    if len(found) != 1:
        raise RuntimeError(f"{text!r} is in the host's chat {len(found)} times")


def negative_control(check, *args) -> str:
    """A deliberately wrong expectation must fail the same check."""
    try:
        check(*args)
    except RuntimeError as error:
        return f"a wrong expectation fails: {error}"
    raise RuntimeError(f"the check accepted a wrong outcome: {args[1:]!r}")


def conversation_decision(journey: TerminalJourney, agent: str, provider_logs: bool) -> list[str]:
    pane = journey.launch("terminal", "desk")
    journey.open_chat(pane, agent)
    journey.type(pane, PROMPT)
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, PROMPT)
    card = journey.wait_terms(pane, "Wants to run", "deploy --check", "1. Yes")
    sent = journey.wait_chat("desk", agent, lambda chat: len(prompts(chat, PROMPT)) >= 1, "prompt-reflected")
    reflected_once(sent, PROMPT)
    control = negative_control(reflected_once, sent, PROMPT + " (a wrong prompt)")
    journey.frame(pane, "permission")
    # The first choice is Yes, this once.
    journey.keys(pane, "1")
    journey.wait(
        pane,
        lambda frame: at_rest(frame, REPLY) and "Wants to run" not in frame,
        "the settled turn",
    )
    settled = journey.wait_chat(
        "desk",
        agent,
        lambda chat: chat["phase"] == "IDLE" and any(REPLY in item["text"] for item in chat["items"]),
        "turn-settled",
    )
    reflected_once(settled, PROMPT)
    assertions = [
        f"{PROMPT!r} reflected once in the desk's chat",
        control,
        "the permission ask offered its choices in the composer's box and Yes was taken",
        f"the reply {REPLY!r} arrived and the desk says idle",
    ]
    if provider_logs:
        lines = journey.provider_input(agent, "provider-input")
        answers = [line for line in lines if "allow" in line.lower() or "accept" in line.lower()]
        if len(answers) != 1:
            raise RuntimeError(f"the provider did not receive one allow: {lines!r}")
        assertions.append("the provider received exactly one allow")
    journey.frame(pane, "settled")
    del card
    journey.quit_client(pane)
    assertions.append("the client exited 0")
    return assertions


def leave_and_recover(journey: TerminalJourney) -> list[str]:
    first = "The recovery state is safely stored."
    second = "A second turn arrived while you were away."
    third = "Back after the restart."
    # A real first run on the laptop makes its replica of the desk's agent.
    pane = journey.launch("first-run", "laptop")
    journey.open_chat(pane, "keeper")
    journey.wait_terms(pane, first)
    journey.wait_chat("laptop", "keeper", lambda chat: any(first in i["text"] for i in chat["items"]), "replica-made")
    journey.quit_client(pane)

    # The desk goes out of reach; a fresh client opens from the laptop's
    # store, with the composer waiting and the draft kept.
    journey.request({"Sever": {"a": "desk", "b": "laptop"}})
    pane = journey.launch("terminal", "laptop")
    journey.open_chat(pane, "keeper")
    journey.wait_terms(pane, first, "desk away · not current")
    journey.type(pane, DRAFT)
    journey.wait_terms(pane, DRAFT, "Draft kept · sending waits")
    journey.frame(pane, "cached-offline")

    # Work goes on at the desk while the laptop is cut off; when the link
    # returns the delta appends once and the composer is live again.
    journey.request({"Send": {"agent": "keeper", "text": "Carry on."}})
    journey.wait_chat("desk", "keeper", lambda chat: any(second in i["text"] for i in chat["items"]), "desk-moved-on")
    journey.request({"Restore": {"a": "desk", "b": "laptop"}})
    journey.wait(
        pane,
        lambda frame: second in frame and "not current" not in frame and DRAFT in frame,
        "the reconciled chat with the draft kept",
    )
    desk = journey.chat("desk", "keeper", "desk-after-restore")
    laptop = journey.wait_chat(
        "laptop",
        "keeper",
        lambda chat: [i["key"] for i in chat["items"]] == [i["key"] for i in desk["items"]],
        "laptop-matches-desk",
    )
    if sum(second in item["text"] for item in laptop["items"]) != 1:
        raise RuntimeError(f"the delta arrived other than once: {laptop!r}")
    journey.frame(pane, "reconciled")

    # The laptop's own daemon restarts under the open chat: the client
    # reconnects by itself, keeps the chat and the draft, and sends.
    journey.request({"RestartDaemon": {"host": "laptop"}})
    journey.wait(
        pane,
        lambda frame: DRAFT in frame and "Draft kept" not in frame and second in frame,
        "the chat live again after the restart",
    )
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, third)
    after = journey.wait_chat("desk", "keeper", lambda chat: any(third in i["text"] for i in chat["items"]), "sent-after-restart")
    reflected_once(after, DRAFT)
    journey.frame(pane, "after-restart")
    # The desk rewinds: the laptop's replica is Reset, and the rows stay on
    # screen until the rebuilt transcript swaps in.
    journey.request({"Checkpoint": {"host": "desk"}})
    journey.request({"Rewind": {"host": "desk", "cuts": []}})
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        screen = journey.capture(pane)
        if first not in screen or third not in screen:
            raise RuntimeError(f"rows left the screen during the Reset:\n{screen}")
        time.sleep(0.05)
    desk = journey.chat("desk", "keeper", "desk-after-rewind")
    journey.wait_chat(
        "laptop",
        "keeper",
        lambda chat: [i["key"] for i in chat["items"]] == [i["key"] for i in desk["items"]],
        "laptop-after-reset",
    )
    # A restored drive has no running processes: the agent reads exited.
    journey.wait(pane, lambda frame: "not current" not in frame and third in frame and "enter resume" in frame, "the chat after the Reset")
    journey.frame(pane, "after-reset")

    journey.quit_client(pane)
    return [
        "a fresh client painted the cached chat from the laptop's store with the desk away and the composer waiting",
        "the draft typed while detached was kept through the outage and the daemon restart",
        "the desk's second turn appended once when the link returned; the laptop holds exactly the desk's items",
        "after the laptop's daemon restarted the client reconnected on its own and the draft was sent and answered",
        "rows stayed on screen through the desk's rewind and Reset until the rebuilt transcript swapped in",
        "the client exited 0",
    ]


def listed(agents: list[dict], agent_id: str) -> dict | None:
    return next((agent for agent in agents if agent["id"] == agent_id), None)


def lifecycle(agents: list[dict], agent_id: str, name: str, state: int) -> None:
    """The host lists `agent_id` under `name` in lifecycle `state`."""
    agent = listed(agents, agent_id)
    if agent is None or agent["name"] != name or agent["lifecycle"] != state:
        raise RuntimeError(f"the host lists {agent_id} as {agent!r}, not {name!r} in lifecycle {state}")


LIVE, EXITED = 1, 2
HELLO = "Say hello."
BACK = "Welcome back."
READY = "Ready when you are."


def manage_agent(journey: TerminalJourney) -> list[str]:
    before = {agent["id"] for agent in journey.inventory("desk", "inventory-before")}
    pane = journey.launch("terminal", "desk")
    journey.wait_terms(pane, "keeper", "n new")
    # Started from home with its first prompt: a Claude the desk runs
    # headless, whose chat opens on the first reply.
    journey.keys(pane, "n")
    journey.wait_terms(pane, "What should the new agent work on?", "Claude")
    journey.type(pane, HELLO)
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: chat_of(frame) is not None and at_rest(frame, HELLO, READY), "the first reply")
    created = journey.wait_inventory(
        "desk", lambda agents: len({a["id"] for a in agents} - before) == 1, "created"
    )
    (agent_id,) = {agent["id"] for agent in created} - before
    made = listed(created, agent_id)
    if made is None or made["lifecycle"] != LIVE:
        raise RuntimeError(f"the desk lists the new agent as {made!r}")
    journey.frame(pane, "created")

    # Renamed in place on home: the host lists the same agent under the name.
    journey.home(pane)
    journey.select_agent(pane, "unnamed")
    journey.keys(pane, "r", "C-u")
    journey.type(pane, "helper")
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: agent_row(frame, "helper") is not None, "helper on home")
    lifecycle(
        journey.wait_inventory("desk", lambda agents: (listed(agents, agent_id) or {}).get("name") == "helper", "renamed"),
        agent_id,
        "helper",
        LIVE,
    )

    # Stopped: exited on the host with its history kept, and resumable.
    journey.select_agent(pane, "helper")
    journey.keys(pane, "s")
    journey.wait_terms(pane, "Stop helper? It can be resumed later.")
    journey.frame(pane, "stop-asked")
    journey.keys(pane, "y")
    journey.wait(pane, lambda frame: "▸ Exited 1 " in frame and agent_row(frame, "helper") is None, "helper exited")
    stopped = journey.wait_inventory(
        "desk", lambda agents: (listed(agents, agent_id) or {}).get("lifecycle") == EXITED, "stopped"
    )
    lifecycle(stopped, agent_id, "helper", EXITED)
    control = negative_control(lifecycle, stopped, agent_id, "helper", LIVE)
    kept = journey.chat("desk", agent_id, "history-kept")
    reflected_once(kept, HELLO)
    if not any(READY in item["text"] for item in kept["items"]):
        raise RuntimeError(f"the stopped agent's history lost its reply: {kept!r}")
    journey.open_exited(pane)
    journey.open_chat(pane, "helper")
    journey.wait_terms(pane, HELLO, READY, "enter resume")
    journey.frame(pane, "exited-and-resumable")

    # Resumed from the exited composer: the same identity, live again.
    journey.type(pane, BACK)
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: frame.count(READY) == 2 and not working(frame), "the resumed reply")
    lifecycle(
        journey.wait_inventory("desk", lambda agents: (listed(agents, agent_id) or {}).get("lifecycle") == LIVE, "resumed"),
        agent_id,
        "helper",
        LIVE,
    )
    resumed = journey.wait_chat("desk", agent_id, lambda chat: len(prompts(chat, BACK)) == 1, "resumed-chat")
    reflected_once(resumed, HELLO)
    reflected_once(resumed, BACK)
    journey.frame(pane, "resumed")

    # Deleted, explicitly: gone from home and from the host.
    journey.home(pane)
    journey.select_agent(pane, "helper")
    journey.keys(pane, "x")
    journey.wait_terms(pane, "Delete helper and its history?")
    journey.keys(pane, "y")
    journey.wait(pane, lambda frame: agent_row(frame, "helper") is None, "home without helper")
    journey.wait_inventory("desk", lambda agents: listed(agents, agent_id) is None, "deleted")
    journey.frame(pane, "deleted")
    journey.quit_client(pane)
    return [
        "n, a first prompt and enter started a headless Claude the desk lists, and its chat opened on the reply",
        "r renamed it in place; the desk lists the same id as helper",
        "s stopped it: the desk lists it exited, with its prompt and reply kept",
        control,
        "a message from the exited composer resumed the same id, and each prompt is in the desk's chat once",
        "x deleted it: gone from home and from the desk",
        "the client exited 0",
    ]


def pin_of(frame: str) -> str:
    found = re.search(r"Pairing PIN: (\d{3}) (\d{3})", frame)
    if found is None:
        raise RuntimeError(f"no PIN on the desk:\n{frame}")
    return found.group(1) + found.group(2)


def identified(frame: str, host_id: str) -> None:
    """The laptop names the desk by the desk's own host id."""
    if f"Paired with desk ({host_id}) on this network." not in frame:
        raise RuntimeError(f"the pairing did not name desk {host_id}:\n{frame}")


def reach_host(journey: TerminalJourney) -> list[str]:
    desk_id = journey.host_id("desk")
    prompt = "Hello from the laptop."
    # Unpaired: the laptop finds the desk and says how to pair with it.
    pane = journey.launch("terminal", "laptop")
    journey.wait(pane, at_home, "home")
    journey.keys(pane, "p")
    journey.wait_terms(pane, "desk", "found nearby · amux pair desk")
    journey.frame(pane, "found-not-paired")
    peers = journey.launch("peers", "laptop", "peers")
    listing = journey.wait_terms(peers, "AMUX_EXIT_0")
    if not re.search(rf"^desk\s+{desk_id}\s+found nearby\s+no: amux pair desk$", listing, re.M):
        raise RuntimeError(f"peers does not list the desk by its id as found:\n{listing}")

    # Six digits the desk printed pair the laptop with it.
    opened = journey.launch("pairing", "desk", "pair")
    pin = pin_of(journey.wait_terms(opened, "Pairing PIN:", "Ctrl+C closes it."))
    asking = journey.launch("pair", "laptop", "pair", "desk")
    journey.wait_terms(asking, "Pairing PIN shown on the other host:")
    journey.type(asking, pin)
    journey.keys(asking, "Enter")
    paired = journey.wait_terms(asking, "Paired with desk", "AMUX_EXIT_0")
    identified(paired, desk_id)
    control = negative_control(identified, paired, "00000000-0000-0000-0000-000000000000")
    journey.wait_terms(opened, "Pairing mode ended.", "AMUX_EXIT_0")
    desk_peers = journey.launch("desk-peers", "desk", "peers")
    trusted = journey.wait_terms(desk_peers, "AMUX_EXIT_0")
    laptop_id = journey.host_id("laptop")
    if not re.search(rf"^laptop\s+{laptop_id}\s.*\syes$", trusted, re.M):
        raise RuntimeError(f"the desk does not list the laptop as paired:\n{trusted}")

    # The running client's overlay says so, and the desk's work is there.
    journey.wait(
        pane,
        lambda frame: "found nearby" not in frame and re.search(r"desk\s+direct", frame) is not None,
        "the desk paired in the overlay",
    )
    journey.frame(pane, "paired")
    journey.keys(pane, "Escape")
    journey.open_chat(pane, "desk-work")
    journey.type(pane, prompt)
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: at_rest(frame, "The desk is reachable."), "the desk's reply")
    heard = journey.wait_chat("desk", "desk-work", lambda chat: chat["phase"] == "IDLE" and len(prompts(chat, prompt)) == 1, "desk-heard")
    reflected_once(heard, prompt)
    journey.frame(pane, "usable-chat")
    journey.quit_client(pane)
    return [
        "the unpaired laptop listed the desk as found, with `amux pair desk`, and `amux peers` named it by its id",
        f"the PIN the desk printed paired the laptop, which named desk {desk_id}",
        control,
        "the desk lists the laptop as paired",
        "the running client's overlay showed the desk online and direct without a restart",
        f"the desk's agent opened from the laptop; the desk holds {prompt!r} once and its reply",
        "the client left the chat and exited 0",
    ]


PASTED = "\n".join(f"deploy log {n}: rsync finished with status 0" for n in range(12))
COMMENT = "Why --delete here?"


def reviewed(chat: dict, comment: str) -> dict:
    """The one prompt carrying the paste and the review, as the desk holds it."""
    sent = [item for item in chat["items"] if item["attachments"]]
    if len(sent) != 1:
        raise RuntimeError(f"the desk holds {len(sent)} prompts with attachments")
    item = sent[0]
    kinds = [attachment["kind"] for attachment in item["attachments"]]
    if item["text"] != "Please check \ufffc against \ufffc" or kinds != ["text", "review"]:
        raise RuntimeError(f"the prompt's order is not text, paste, text, review: {item!r}")
    if item["attachments"][0]["text"] != PASTED:
        raise RuntimeError("the pasted text arrived changed")
    comments = item["attachments"][1]["comments"]
    if comments != [{"path": "deploy.sh", "line": 3, "old_line": 0, "text": comment}]:
        raise RuntimeError(f"the review's comments are {comments!r}")
    return item


def attachment_or_review(journey: TerminalJourney) -> list[str]:
    # The reviewer's working tree on the desk: one commit and an edit.
    work = Path(journey.ready["root"]) / "desk" / "work"

    # A pinned author and date keep the commit id the review page shows.
    pinned = {"GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z", "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z"}

    def git(*args: str) -> None:
        subprocess.run(
            ["git", "-C", str(work), "-c", "user.name=Journey", "-c", "user.email=journey@example.invalid", *args],
            check=True,
            capture_output=True,
            env={**os.environ, **pinned},
        )

    git("init", "-q", "-b", "main")
    (work / "deploy.sh").write_text("#!/bin/sh\necho deploying\nrsync build/ prod:/srv\n")
    git("add", "deploy.sh")
    git("commit", "-qm", "Deploy by rsync")
    (work / "deploy.sh").write_text("#!/bin/sh\necho deploying\nrsync --delete build/ prod:/srv\necho done\n")

    # On the laptop: text, a long paste as one token, text, and a review of
    # the desk's working tree with one comment.
    pane = journey.launch("terminal", "laptop")
    journey.open_chat(pane, "reviewer")
    journey.type(pane, "Please check ")
    journey.paste(pane, PASTED)
    journey.wait_terms(pane, "Please check [pasted-1 · 12 lines]")
    # The chip is followed by a space of its own.
    journey.type(pane, "against ")
    journey.keys(pane, "C-a", "r")
    journey.wait_terms(pane, "review · working tree at", "deploy.sh  +2 −1")
    for _ in range(12):
        if re.search(r"│▌\s+3 \+ rsync --delete", journey.capture(pane)):
            break
        journey.keys(pane, "j")
    else:
        raise RuntimeError(f"no line to comment on:\n{journey.capture(pane)}")
    journey.frame(pane, "review-diff")
    journey.keys(pane, "c")
    journey.type(pane, COMMENT)
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, "1 file · +2 −1 · 1 comment", f"│ {COMMENT}")
    journey.frame(pane, "review-comment")
    journey.keys(pane, "q")
    journey.wait_terms(pane, "Please check [pasted-1 · 12 lines] against [review · 1 comment]")
    journey.frame(pane, "composer-tokens")
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: at_rest(frame, "I read the review."), "the reviewer's reply")

    # The desk received the tokens in order, the paste's exact text, the
    # comment, and the patch it made itself, whose bytes are on its disk.
    received = journey.wait_chat("desk", "reviewer", lambda chat: chat["phase"] == "IDLE" and any(i["attachments"] for i in chat["items"]), "received")
    item = reviewed(received, COMMENT)
    control = negative_control(reviewed, received, COMMENT + " (a wrong comment)")
    patch = item["attachments"][1]["patch"]
    found = [path for path in (Path(journey.ready["root"]) / "desk" / "data").rglob(patch) if path.parent.name == "blobs"]
    if len(found) != 1:
        raise RuntimeError(f"the desk holds the patch {patch} {len(found)} times")
    bytes_ = found[0].read_bytes()
    if hashlib.sha256(bytes_).hexdigest() != patch or b"+rsync --delete build/ prod:/srv" not in bytes_:
        raise RuntimeError("the patch on the desk is not the reviewed diff")

    # Revisited: the chat opened again shows the paste's opening lines in
    # place and the review as its token.
    journey.home(pane)
    journey.open_chat(pane, "reviewer")
    journey.wait_terms(pane, "Please check", "deploy log 0:", "against [review · 1 comment]", "I read the review.")
    journey.frame(pane, "reopened")
    journey.quit_client(pane)
    return [
        "the prompt reached the desk as text, the paste, text and the review, in that order",
        "the paste arrived as one token holding exactly the twelve lines pasted",
        f"the review carries one comment on deploy.sh line 3: {COMMENT!r}",
        control,
        "the review names a patch the desk computed; its bytes on the desk hash to that name and hold the edit",
        "the chat opened again shows the paste in place, the review token and the reply",
        "the client exited 0",
    ]


def never_received(chat: dict, text: str) -> None:
    if prompts(chat, text):
        raise RuntimeError(f"the desk received {text!r} while access was lost")


def keep_authority(journey: TerminalJourney) -> list[str]:
    asked = "Are you there?"
    held = "Still there?"
    # The two machines are one account's, reached only through the relay.
    journey.request({"Trust": {"a": "desk", "b": "laptop"}})
    journey.request({"Trust": {"a": "laptop", "b": "desk"}})

    # A second profile on the laptop is a fleet of its own.
    created = journey.launch("profile", "laptop", "profile", "create", "work")
    journey.wait_terms(created, "Created profile work", "AMUX_EXIT_0")
    other = journey.launch("other", "laptop", "--profile", "work")
    journey.wait_terms(other, "no agents yet")
    journey.keys(other, "p")
    journey.wait(other, lambda frame: re.search(r"laptop\s+this machine", frame) is not None and "desk" not in frame, "hosts without the desk")
    journey.frame(other, "other-profile")
    journey.keys(other, "Escape")
    journey.quit_client(other)

    # The default profile reaches the desk's agent through the relay.
    pane = journey.launch("terminal", "laptop")
    journey.open_chat(pane, "guarded")
    journey.type(pane, asked)
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: at_rest(frame, "Only this account reaches me."), "the first reply")
    reflected_once(journey.wait_chat("desk", "guarded", lambda chat: chat["phase"] == "IDLE", "reached"), asked)

    # Signed out, the laptop has no way to the desk: the chat says so and
    # a message written now is held, never sent.
    signed_out = journey.launch("logout", "laptop", "logout")
    journey.wait_terms(signed_out, "Signed default out", "AMUX_EXIT_0")
    # The reason named is this machine's sign-out, never a claim about the
    # desk, and the hosts overlay says the same.
    journey.wait_terms(pane, "desk away · this machine is signed out", "until this machine signs in")
    journey.home(pane)
    journey.keys(pane, "p")
    journey.wait_terms(pane, "signed out · amux login", "offline · this machine is signed out")
    journey.frame(pane, "blocked-hosts")
    journey.keys(pane, "Escape")
    journey.open_chat(pane, "guarded")
    journey.wait_terms(pane, "desk away · this machine is signed out")
    journey.type(pane, held)
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, held, "Draft kept · sending waits until this machine signs in")
    journey.frame(pane, "blocked")
    time.sleep(2)
    blocked = journey.chat("desk", "guarded", "while-signed-out")
    never_received(blocked, held)
    control = negative_control(never_received, blocked, asked)

    # Signed in again: the chat is current and the held message goes once.
    journey.request({"SignIn": {"host": "laptop", "account": "ada"}})
    journey.wait(pane, lambda frame: "not current" not in frame and held in frame and "Draft kept" not in frame, "the chat current again")
    journey.keys(pane, "Enter")
    journey.wait(pane, lambda frame: at_rest(frame, "The relay carries us again."), "the reply after signing in")
    after = journey.wait_chat("desk", "guarded", lambda chat: chat["phase"] == "IDLE" and len(prompts(chat, held)) == 1, "sent-after-sign-in")
    reflected_once(after, asked)
    reflected_once(after, held)
    journey.frame(pane, "recovered")
    journey.quit_client(pane)
    return [
        "a second profile on the laptop opened on its own empty fleet, knowing no desk",
        f"through the relay the desk received {asked!r} once and answered",
        "signed out, the laptop named its own sign-out as the reason the desk was away, in the chat and the hosts overlay, and held the message; the desk never received it",
        control,
        f"signed in again, the chat was current and {held!r} reached the desk once and was answered",
        "the clients exited 0",
    ]


STORIES = {
    "conversation-decision-claude-pty": lambda j: conversation_decision(j, "decision-pty", False),
    "conversation-decision-claude-sdk": lambda j: conversation_decision(j, "decision-sdk", True),
    "conversation-decision-codex": lambda j: conversation_decision(j, "decision-codex", True),
    "leave-and-recover": leave_and_recover,
    "manage-agent": manage_agent,
    "reach-host": reach_host,
    "attachment-or-review": attachment_or_review,
    "keep-authority": keep_authority,
}


def main() -> int:
    if len(sys.argv) != 2 or sys.argv[1] not in STORIES:
        print(f"usage: terminal-journey.py {{{','.join(STORIES)}}}", file=sys.stderr)
        return 2
    name = sys.argv[1]
    declared = story(name)
    journey = TerminalJourney(declared, ROOT / declared["topology"])
    try:
        assertions = STORIES[name](journey)
        journey.finish(assertions)
        print(f"PASS {name}")
        for assertion in assertions:
            print(f"- {assertion}")
        return 0
    except BaseException as error:
        journey.fail(error)
        print(f"FAIL {name}: {error}", file=sys.stderr)
        return 1
    finally:
        journey.close()


if __name__ == "__main__":
    sys.exit(main())
