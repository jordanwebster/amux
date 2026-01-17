# Claude Code Instructions - Dashboard

This is a React dashboard for amux that displays Claude agent conversations in a ChatGPT/Claude-like UI.

## Tech Stack

- React 18 + TypeScript + Vite
- Tailwind CSS (with CSS variables for theming)
- Zustand for state management
- react-markdown + react-syntax-highlighter for markdown rendering
- WebSocket connection to amux (port 9002)

## Project Structure

```
src/
├── components/
│   ├── ui/              # Base UI components (scroll-area)
│   ├── AgentSidebar.tsx # Left sidebar with agent list
│   ├── ChatWindow.tsx   # Message display area
│   ├── Message.tsx      # Individual message (user/assistant styling)
│   ├── MarkdownContent.tsx # Markdown renderer with code blocks
│   └── InputArea.tsx    # Bottom input (currently disabled)
├── hooks/
│   └── useWebSocket.ts  # WebSocket connection and message handling
├── store/
│   └── appStore.ts      # Zustand store
├── types/
│   └── protocol.ts      # TypeScript types matching amux protocol
└── lib/
    └── utils.ts         # cn() helper
```

## Development

```bash
npm install
npm run dev
```

The dashboard connects to amux WebSocket on `ws://localhost:9002`.

## Protocol Notes

The amux WebSocket protocol uses JSON with serde's default enum format:
- Most messages: `{"VariantName": {data}}` or `"VariantName"` for unit variants
- Exception: `StructuredLog` uses `#[serde(tag = "type")]` format: `{type: "UserMessage", content: "...", ...}`

Key message flow:
1. Connect: `{"Connect": {"host_id": "..."}}`
2. List agents: `"ListAgents"` → `{"ListAgentsResult": {"agents": [...]}}`
3. Subscribe: `{"Subscribe": {...}}` → streams `{"StructuredOutput": {...}}`

## Verifying Changes

### Using Playwright MCP

The Playwright MCP server is configured for browser automation testing.

```bash
# Start the dev server
npm run dev

# Use Playwright tools to interact with the dashboard:
# - mcp__playwright__browser_navigate to load the page
# - mcp__playwright__browser_snapshot to see page state
# - mcp__playwright__browser_click to interact with elements
# - mcp__playwright__browser_take_screenshot to capture visuals
# - mcp__playwright__browser_console_messages to check for errors
```

### Testing with amux via tmux

To test the full flow with a real Claude agent:

```bash
# 1. Create a tmux session
tmux new-session -d -s amux-test

# 2. Start or attach to an amux agent
tmux send-keys -t amux-test 'cd /Users/jlw/source/amux && cargo run -- attach -t <agent-name>' Enter

# 3. Wait for attach, then send messages to Claude
# IMPORTANT: Send message and Enter in SEPARATE commands
tmux send-keys -t amux-test 'Your message to Claude here'
tmux send-keys -t amux-test Enter

# 4. Check the output
sleep 5
tmux capture-pane -t amux-test -p

# 5. Clean up when done
tmux kill-session -t amux-test
```

**Why separate send-keys calls?** tmux batches all keys from a single `send-keys` command into one write(). Claude Code distinguishes between:
- Enter with other characters → newline (multi-line input)
- Enter alone → submit message

### Verification Checklist

- [ ] WebSocket connects (status shows "connected")
- [ ] Agent list populates in sidebar
- [ ] Clicking agent subscribes and loads messages
- [ ] User messages: right-aligned dark bubbles
- [ ] Assistant messages: left-aligned, no bubble, markdown rendered
- [ ] Code blocks have syntax highlighting
- [ ] New messages auto-scroll into view
- [ ] Light/dark mode follows system preference

## UI Design

Based on the Claude UI:
- Dark sidebar (always dark regardless of theme)
- User messages: right-aligned with dark pill/bubble
- Assistant messages: left-aligned, plain text with markdown support
- Minimal design - no unnecessary chrome

## Known Limitations

- Input is read-only (WebSocket protocol doesn't support sending input yet)
- Only connects to localhost:9002
- No reconnection logic on disconnect
