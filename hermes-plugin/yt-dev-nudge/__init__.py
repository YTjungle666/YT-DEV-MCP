"""Nudge the model to use yt-dev tools, as parts or as the full flow.

The yt-dev server has no prompt presence of its own and its tools are deferred,
so nothing makes the model reach for them. This registers pre_llm_call and
returns the routing block on the first turn, then every N turns, so it survives
long sessions and context compaction.
"""

from __future__ import annotations

from collections import OrderedDict

NUDGE = """<yt_dev_routing>
写代码/改项目时按需挑件用，不一定需要 ~/Plan 计划：
- 有计划的 ~/Plan/<项目>：先 dev_projects 看哪几摊在动、dev_plan 读该项目的状态；
  新项目用 dev_flow init 走标准流程（plan→execute→review→gate→release→publish）。
- 没有计划：dev_gate / dev_review / dev_release / dev_publish / dev_worker 都能单独用，
  传 path=<仓库绝对路径>，不需要 PLAN.md、不用登记项目。
- 本会话确实用不上（纯问答、只读排查别人的环境等）：在第一句回复里写明「不适用 yt-dev」，
  然后照常干活——不许默默跳过。
</yt_dev_routing>"""

DEFAULT_EVERY_N = 10
_CAP = 1000


def register(ctx) -> None:
    turns: "OrderedDict[str, int]" = OrderedDict()

    def every_n() -> int:
        try:
            n = int(ctx.get_config("reinject_every_n", DEFAULT_EVERY_N))
        except Exception:
            return DEFAULT_EVERY_N
        return max(0, n)

    def pre_llm_call(session_id: str = "", is_first_turn: bool = False, **_kwargs):
        key = session_id or "unknown"
        turn = turns[key] = turns.get(key, 0) + 1
        turns.move_to_end(key)
        while len(turns) > _CAP:
            turns.popitem(last=False)
        if is_first_turn:
            return {"context": NUDGE}
        n = every_n()
        if n and turn % n == 0:
            return {"context": NUDGE}
        return None

    ctx.register_hook("pre_llm_call", pre_llm_call)
