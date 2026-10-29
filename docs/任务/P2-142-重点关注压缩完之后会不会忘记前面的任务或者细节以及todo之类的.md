# 任务：重点关注压缩完之后会不会忘记前面的任务或者细节以及todo之类的

2026-10-29，用户在看板上新建。

## 用户原话

## 要做的

## 怎么算做完



下面是claude code压缩之后的重新读取的显示：


Bash(git branch p2-135-late-display p2-135-lsp-tui && git log --oneline -1 p2-135-late-display | cat)
0eabaee fix(P2-135): 60 秒只限制等结果，已到的诊断留到下一次请求交给模型

⏺ Bash(cd /Users/firegnu/Developer/personal_projs/canter-worktrees/p2-135-lsp-tui && export CARGO_TARGET_DIR=$HOME/Developer/personal_projs/canter-worktrees/.target/p2-135-lsp-tui; O=/private/tmp/claude-501/-Users-firegnu-Developer-personal-projs-canter/518c94fa-5126-46d6-b42f-c7e9fe0e9c0d/scratchpad/e2dbe79-suite.txt; { cargo nextest run --all-targets --no-fail-fast 2>&1 | grep -E "FAIL|Summary|Starting"; cargo test --doc --workspace 2>&1 | grep -E "^test result" | awk '{p+=$4; f+=$6} END {print "DOC passed",p,"failed",f}'; cargo clippy --all-targets -q -- -D warnings 2>&1 | tail -3 && echo CLIPPY_DONE; cargo fmt --check && echo FMT_OK; } > $O 2>&1; cat $O)
Read docs/任务/P2-136-LSP接第一批语言.md (60 lines)
Referenced file ../canter-worktrees/p2-135-lsp-tui/docs/DESIGN.md
Read ../../../../../private/tmp/claude-501/-Users-firegnu-Developer-personal-projs-canter/518c94fa-5126-46d6-b42f-c7e9fe0e9c0d/scratchpad/p2-135-checks2.txt (17 lines)
Referenced file ../canter-worktrees/p2-135-lsp-tui/crates/canter-core/tests/lsp.rs
Read ../../../../../private/tmp/claude-501/-Users-firegnu-Developer-personal-projs-canter/518c94fa-5126-46d6-b42f-c7e9fe0e9c0d/scratchpad/p2-135-checks.txt (12 lines)
Skills restored (corral-dispatch)
     (ctrl+b to run in background)

## 主控记下的（10-29）

- 10-29 主控为长程任务又过了一遍；用户：「整体退回也排到第三期，压缩好几次以后会不会走样这个要排到第二期。」并进本件：长任务会压缩不止一次，摘要的摘要可能越压越偏。本件要专门看连续压缩两三次以后，任务、细节、待办还在不在。
- 要看的东西参考 P2-81「压缩后不丢东西」那一节：用户中途定的规矩、试过失败的办法、改过哪些文件和为什么、后台命令的编号和在等什么。
