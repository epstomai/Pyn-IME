"""Engines the evaluation harness can drive.

Contract (see docs/eval-protocol.md):

    reset()                 forget everything learned, clear the context
    set_context(segments)   the text before the caret now reads this;
                            NOT a learning event; returns False if the
                            engine could not take the context as given
    convert(keys, n)        candidates for raw keystrokes, best first
    commit(text)            the user picked `text` for the last keys;
                            a learning event; returns False if the engine
                            could not produce that text
    undo()                  the user deleted what was just committed;
                            returns False if unsupported

A context segment is {"text": str, "keys": str | None}; `keys` is what the
user typed to produce a hanzi run, None for punctuation and other text.
"""
from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path


class Engine:
    name = "engine"
    capabilities = {"context": False, "learn": False, "undo": False}
    # Engines that can only emulate a context by committing text wipe what
    # that taught them before the next case. The pollution suite turns this
    # off, because there the learned state is the thing under test.
    isolate_contexts = True

    def begin_case(self, case: dict) -> None:
        """Harness hook; real engines never look at the case."""

    def reset(self) -> None:
        raise NotImplementedError

    def set_context(self, segments: list[dict]) -> bool:
        raise NotImplementedError

    def convert(self, keys: str, n: int) -> list[str]:
        raise NotImplementedError

    def commit(self, text: str) -> bool:
        raise NotImplementedError

    def undo(self) -> bool:
        return False

    def checkpoint(self) -> None:
        """Save learned state so context emulation can restore back to it."""

    def restore_checkpoint(self) -> None:
        """Restore the state saved by the last checkpoint(), if dirty."""

    def close(self) -> None:
        pass


def context_text(segments: list[dict]) -> str:
    return "".join(seg["text"] for seg in segments)


class OracleEngine(Engine):
    """Returns the expected answer. Must score 100%: it tests the harness."""

    name = "oracle"
    capabilities = {"context": True, "learn": False, "undo": False}

    def __init__(self):
        self._answer = None

    def begin_case(self, case: dict) -> None:
        self._answer = case.get("expected") or case.get("example")

    def reset(self) -> None:
        self._answer = None

    def set_context(self, segments: list[dict]) -> bool:
        return True

    def convert(self, keys: str, n: int) -> list[str]:
        return [self._answer] if self._answer else []

    def commit(self, text: str) -> bool:
        return True


class EchoEngine(Engine):
    """Returns the keystrokes unchanged. Must fail: it tests the harness."""

    name = "echo"

    def reset(self) -> None:
        pass

    def set_context(self, segments: list[dict]) -> bool:
        return not segments

    def convert(self, keys: str, n: int) -> list[str]:
        return [keys]

    def commit(self, text: str) -> bool:
        return False


class ProcessEngine(Engine):
    """An engine process speaking the JSONL protocol on stdin/stdout."""

    def __init__(self, command: list[str], name: str | None = None):
        # CreateProcess does not resolve a relative path with forward slashes.
        program = Path(command[0])
        if program.exists():
            command = [str(program.resolve()), *command[1:]]
        self._proc = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            encoding="utf-8",
            bufsize=1,
        )
        hello = self._call({"op": "hello"})
        self.name = name or hello.get("name", "process")
        self.capabilities = hello.get("capabilities", dict(Engine.capabilities))

    def _call(self, request: dict) -> dict:
        self._proc.stdin.write(json.dumps(request, ensure_ascii=False) + "\n")
        self._proc.stdin.flush()
        line = self._proc.stdout.readline()
        if not line:
            raise RuntimeError(f"engine process exited (request {request!r})")
        reply = json.loads(line)
        if not reply.get("ok", False):
            raise RuntimeError(f"engine error for {request!r}: {reply}")
        return reply

    def reset(self) -> None:
        self._call({"op": "reset"})

    def set_context(self, segments: list[dict]) -> bool:
        return bool(self._call({"op": "context", "segments": segments}).get("accepted", True))

    def convert(self, keys: str, n: int) -> list[str]:
        return list(self._call({"op": "convert", "keys": keys, "n": n})["candidates"])

    def commit(self, text: str) -> bool:
        return bool(self._call({"op": "commit", "text": text}).get("accepted", True))

    def undo(self) -> bool:
        return bool(self._call({"op": "undo"}).get("accepted", False))

    def close(self) -> None:
        try:
            self._call({"op": "bye"})
        except Exception:
            pass
        self._proc.stdin.close()
        self._proc.wait(timeout=10)


# ASCII key that makes Rime's punctuator emit the given full-width mark.
_PUNCT_KEYS = {"，": ",", "。": ".", "？": "?", "！": "!", "；": ";", "：": ":"}
_PICK_SCAN = 300


class RimeEngine(Engine):
    """librime with a schema, as the measured baseline.

    Rime has no API for surrounding text, so a context is reproduced the way
    a user would create it: type the keys and pick candidates until the text
    matches. That teaches Rime's user dictionary, so the learned state is
    wiped before the next context is set.
    """

    capabilities = {"context": True, "learn": True, "undo": False}

    def __init__(self, dist_dir: Path, user_dir: Path, schema: str, name: str):
        from rime_ctypes import Rime

        self.name = name
        self._schema = schema
        self._user_dir = Path(user_dir).resolve()
        self._checkpoint_dir = self._user_dir.parent / f".ckpt_{self.name}"
        log_dir = self._user_dir.parent / "rime_logs" / name
        log_dir.mkdir(parents=True, exist_ok=True)
        self._rime = Rime(dist_dir, self._user_dir, log_dir=log_dir)
        self._wipe_state()
        self._rime.start(full_check=True)
        self._select_schema()
        self._dirty = False
        self._keys = ""
        self.context_failures = 0

    def _select_schema(self) -> None:
        if not self._rime.select_schema(self._schema) or self._rime.schema() != self._schema:
            raise RuntimeError(f"cannot select Rime schema {self._schema!r}")

    def _wipe_state(self) -> None:
        for path in self._user_dir.glob("*.userdb"):
            shutil.rmtree(path)

    def _wipe_checkpoint(self) -> None:
        if self._checkpoint_dir.exists():
            shutil.rmtree(self._checkpoint_dir)

    def version(self) -> str:
        return self._rime.version()

    def reset(self) -> None:
        self._rime.stop()
        self._wipe_state()
        self._wipe_checkpoint()
        self._rime.start(full_check=False)
        self._select_schema()
        self._dirty = False

    def checkpoint(self) -> None:
        self._rime.stop()
        self._wipe_checkpoint()
        self._checkpoint_dir.mkdir(parents=True, exist_ok=True)
        for path in self._user_dir.glob("*.userdb"):
            shutil.copytree(path, self._checkpoint_dir / path.name)
        self._rime.start(full_check=False)
        self._select_schema()
        self._dirty = False

    def restore_checkpoint(self) -> None:
        if not self._checkpoint_dir.exists() or not self._dirty:
            return
        self._rime.stop()
        self._wipe_state()
        for path in self._checkpoint_dir.glob("*.userdb"):
            shutil.copytree(path, self._user_dir / path.name)
        self._rime.start(full_check=False)
        self._select_schema()
        self._dirty = False

    def _type_and_pick(self, keys: str, target: str) -> bool:
        """Type `keys`, pick candidates to spell `target`, commit. True if exact."""
        rime = self._rime
        rime.clear()
        rime.take_commit()
        if not rime.set_input(keys):
            return False
        produced = ""
        while len(produced) < len(target):
            rest = target[len(produced) :]
            best_index, best_text = -1, ""
            for index, (text, _) in enumerate(rime.candidates(_PICK_SCAN)):
                if rest.startswith(text) and len(text) > len(best_text):
                    best_index, best_text = index, text
            if best_index < 0 or not rime.select(best_index):
                break
            produced += best_text
            committed = rime.take_commit()
            if committed:
                self._dirty = True
                return committed == target
        if produced != target:
            rime.clear()
            return False
        rime.commit_composition()
        self._dirty = True
        return rime.take_commit() == target

    def set_context(self, segments: list[dict]) -> bool:
        if self._dirty and self.isolate_contexts:
            self.reset()
        self._rime.clear()
        accepted = True
        for segment in segments:
            if segment.get("keys"):
                accepted &= self._type_and_pick(segment["keys"], segment["text"])
                continue
            for mark in segment["text"]:
                key = _PUNCT_KEYS.get(mark)
                if key is None:
                    accepted = False
                    continue
                self._rime.process_key(ord(key))
                accepted &= self._rime.take_commit() == mark
                self._dirty = True
        if not accepted:
            self.context_failures += 1
        return accepted

    def convert(self, keys: str, n: int) -> list[str]:
        self._rime.clear()
        self._keys = keys
        if not self._rime.set_input(keys):
            return []
        candidates = [text for text, _ in self._rime.candidates(n)]
        self._rime.clear()
        return candidates

    def commit(self, text: str) -> bool:
        return self._type_and_pick(self._keys, text)

    def close(self) -> None:
        self._rime.stop()
        self._wipe_checkpoint()
