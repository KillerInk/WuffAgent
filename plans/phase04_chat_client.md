# Phase 4: Chat Client

## Status: Pending

---

### Step 4.1: HTTP Request Builder

**Objective: Build HTTP requests for chat completions.

**Tasks:
- Create `internal/client/chat.go`
- Define `Message`, `ChatRequest`, `Response` structs
- Implement `buildRequest()` method
- Serialize to JSON, create HTTP request

```go
package client

import (
    "bytes"
    "context"
    "encoding/json"
    "fmt"
    "net/http"
)

type Message struct {
    Role    string `json:"role"`
    Content string `json:"content"`
}

type ChatRequest struct {
    Model      string    `json:"model"`
    Messages  []Message `json:"messages"`
    Stream    bool     `json:"stream"`
}

type Response struct {
    Choices []Choice `json:"choices"`
}

type Choice struct {
    Message Message `json:"message"`
}

type ChatClient struct {
    BaseURL      string
    SystemPrompt string
    Conversation []Message
    HTTPClient   *http.Client
}

func NewChatClient(baseURL string) *ChatClient {
    return &ChatClient{
        BaseURL:      baseURL,
        HTTPClient: &http.Client{
            Timeout: 120 * time.Second,
        },
    }
}

func (c*ChatClient) buildRequest(prompt string, stream bool) (*ChatRequest, error) {
    req := &ChatRequest{
        Model: "local",
        Stream: stream,
    }
    // Add system prompt
    if c.SystemPrompt != "" {
        req.Messages = append(req.Messages, Message{
            Role: "system",
            Content: c.SystemPrompt,
        })
    }
    // Add conversation history
    req.Messages = append(req.Messages, c.Conversation...)
    // Add current user message
    req.Messages = append(req.Messages, Message{
        Role: "user",
        Content: prompt,
    })
    return req, nil
}
```

**Success Criteria:
- JSON matches OpenAI API format
- Request includes system prompt, message history

**Dependencies: Step 2.1 (config for base URL)

---

### Step 4.2: Non-Streaming Response Handling

**Objective: Send request, receive complete response.

**Tasks:
- Implement `SendMessage()` with `stream: false`
- Parse JSON response
- Return content to caller
- Update conversation history

```go
func (c*ChatClient) SendMessage(prompt string) (*Response, error) {
    req, err := c.buildRequest(prompt, false)
    if err != nil {
        return nil, err
    }

    body, err := json.Marshal(req)
    if err != nil {
        return nil, err
    }

    resp, err := c.HTTPClient.Post(
        fmt.Sprintf("%s/v1/chat/completions", c.BaseURL),
        "application/json",
        bytes.NewBuffer(body),
    )
    if err != nil {
        return nil, fmt.Errorf("HTTP request failed: %w", err)
    }
    defer resp.Body.Close()

    var result Response
    if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
        return nil, fmt.Errorf("failed to parse response: %w", err)
    }

    // Update conversation history
    c.Conversation = append(c.Conversation, Message{
        Role:    "user",
        Content: prompt,
    })
    c.Conversation = append(c.Conversation, Message{
        Role:    "assistant",
        Content: result.Choices[0].Message.Content,
    })

    return &result, nil
}
```

**Success Criteria:
- Complete response text is returned
- Error handling for HTTP failures
- Conversation history is updated

**Dependencies: Step 4.1

---

### Step 4.3: SSE Streaming

**Objective: Stream tokens as they arrive.

**Tasks:
- Implement `StreamMessage()` with `stream: true`
- Parse SSE `data:` lines
- Call callback for each token chunk
- Use goroutine for async updates
- Support cancellation via context
- Handle `[DONE]` marker

```go
func (c*ChatClient) StreamMessage(
    ctx context.Context,
    prompt string,
    callback func(string) error, // token chunk
) error {
    req, err := c.buildRequest(prompt, true)
    if err != nil {
        return err
    }

    body, _ := json.Marshal(req)
    resp, err := c.HTTPClient.Post(
        fmt.Sprintf("%s/v1/chat/completions", c.BaseURL),
        "application/json",
        bytes.NewBuffer(body),
    )
    if err != nil {
        return err
    }
    defer resp.Body.Close()

    // Add user message to history
    c.Conversation = append(c.Conversation, Message{
        Role: "user",
        Content: prompt,
    })

    // Start assistant message placeholder
    var assistantContent string
    c.Conversation = append(c.Conversation, Message{
        Role: "assistant",
        Content: "",
    })

    scanner := bufio.NewScanner(resp.Body)
    for scanner.Scan() {
        line := scanner.Text()

        // SSE parsing
        if strings.HasPrefix(line, "data: ") {
            data := strings.TrimPrefix(line, "data: ")

            if data == "[DONE]" {
                break
            }

            var chunk struct {
                Choices []struct {
                    Delta struct {
                        Content string `json:"content"`
                    } `json:"delta"`
                } `json:"choices"`
            }

            var parsed chunk
            if err := json.Unmarshal([]byte(data), &parsed); err != nil {
                continue
            }

            if len(parsed.Choices) > 0 && parsed.Choices[0].Delta.Content != "" {
                token := parsed.Choices[0].Delta.Content
                assistantContent += token
                if err := callback(token); err != nil {
                    return err
                }
            }
        }
    }

    // Update assistant message in history
    c.Conversation[len(c.Conversation)-1].Content = assistantContent

    return scanner.Err()
}
```

**Success Criteria:
- Callback fires for each token
- Streaming completes on `[DONE]` marker
- Can cancel streaming with context
- Conversation history is updated

**Dependencies: Step 4.1

---

### Step 4.4: Context Window Management

**Objective: Manage conversation history within context limits.

**Tasks:
- Implement history truncation when approaching n_ctx
- Add token estimation (rough estimate: 4 chars = 1 token)
- Warn when history is getting large

```go
func (c*ChatClient) TruncateHistory(maxTokens int) {
    // Remove oldest messages until history fits in context
    // Rough token count
    for len(c.Conversation) > 2 {
        tokens := estimateTokens(c.Conversation)
        if tokens < maxTokens {
            break
        }
        // Remove oldest non-system message
        c.Conversation = c.Conversation[1:] // Keep system prompt at front
    }
}

func estimateTokens(messages []Message) int {
    total := 0
    for _, m := range messages {
        total += len(m.Content) / 4 // Rough estimate
    }
    return total
}
```

**Success Criteria:
- History stays within context window
- User is warned when history is truncated

**Dependencies: Step 4.2

---

### Step 4.5: Stop Generation Mechanism

**Objective: Cancel ongoing generation.

**Tasks:
- `StopGeneration()` closes current HTTP response
- Removes last assistant message from history
- Cleans up conversation state

```go
func (c*ChatClient) StopGeneration() {
    // Close current response body
    // Remove last assistant message from conversation
    // Signal that generation was stopped
}
```

**Success Criteria:
- Generation stops when user clicks stop
- History is cleaned up

**Dependencies: Step 4.3

---

### Step 4.6: Thread Safety for UI Updates

**Objective: Ensure UI updates are thread-safe for Fyne.

**Tasks:
- Document that all callback UI updates must use `fyne.CurrentApp().CallLater()`
- ChatClient callbacks return tokens via goroutine
- UI layer wraps updates in `CallLater()`

**Success Criteria:
- UI updates are thread-safe
- No crashes from goroutine widget updates

**Dependencies: Step 4.3

---

## Files Created:
- `internal/client/chat.go`

## Dependencies on other phases:
- Phase 2 (config provides base URL)
- Phase 8 (streaming integration with UI)

## Review Notes:
- Uses OpenAI-compatible API (`/v1/chat/completions`)
- SSE parsing handles partial lines, JSON per line
- Context cancellation for stop generation
- Thread safety: callbacks must use `app.CurrentApp().CallLater()` for Fyne widget updates
- Token estimation is rough (4 chars = 1 token). More accurate estimation could use tiktoken or llama.cpp token counting if needed
- History truncation removes oldest messages first, keeps system prompt
- Stop generation: close HTTP response, remove last assistant message from history