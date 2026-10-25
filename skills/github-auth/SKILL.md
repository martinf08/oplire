---
name: github-auth
description: Check and use existing GitHub MCP or Git Credential Manager authentication for authorized pushes without exposing tokens.
---

# GitHub authentication and safe pushes

## Credential safety

- Prefer a connected GitHub MCP tool when it is available in the current session.
- For HTTPS Git operations, let Git Credential Manager supply an already-stored credential to Git.
- Never retrieve, print, log, copy, or include a token in a URL, command-line argument, repository file, or chat message.
- Do not inspect credential-manager databases or OpenCode OAuth files for raw secrets.
- If no authentication is available, ask the user to connect the GitHub MCP server or store a credential through a masked prompt and the configured credential manager. Never ask the user to paste a token into chat.
- Do not assume an MCP server is available merely because it is configured elsewhere. Check the current session's tools and, when available, `opencode mcp list`.

## Push procedure

1. Confirm the repository, current branch, clean working tree, remote URL, and commit to publish.
2. Check the remote branch with `git ls-remote` before pushing.
3. Push only the authorized branch with a normal, non-force push, for example:

   ```sh
   git push --set-upstream origin BRANCH
   ```

4. Verify the remote ref points to the intended commit and confirm the local working tree state.
5. Never retry a failed push blindly. First check whether the remote accepted the commit and diagnose authentication or network errors.

## Cleanup

Remove only temporary copies and build artifacts created for the push, and only after success. Preserve repository data and stored credentials; do not erase a user's credential-manager entry as cleanup.
