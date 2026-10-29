# 任务：接入GPT订阅支持

2026-10-27，用户在看板上新建。

## 用户原话

## 要做的

## 怎么算做完

## 主控记下的（10-27）

- 10-27 用户：「把搜网页记进P2-92， 子agent写到P2-78,applu_patch写到P2-80,盯输出、目标续跑：P2-81 长任务可靠性分解时参考 这个单独写一个task。」
- **apply_patch 跟这件一起做**：DESIGN 第 6、10 条已定，接 OpenAI 时搬 Codex 的 `apply-patch`。GPT 模型习惯用补丁一次增、改、删多个文件：Codex 模型清单里 11 个模型都开 `apply_patch`（freeform），opencode 只给 GPT 模型 `patch`（P2-88 报告 [C9]、[O7]）。
- 搬代码照 CLAUDE.md：文件开头注明来源（项目、版本标签、原文件），保留许可证声明，按 Apache-2.0 维护 `NOTICE`。
- 分解时要定：只给 GPT 模型还是都给；和现有 `write`、`edit` 的批准、改动记录（P2-54）、撤销（P2-23）怎么接。
