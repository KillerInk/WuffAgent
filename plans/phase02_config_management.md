# Phase 2: Config Management

## Status: Pending

---

### Step 2.1: Config Struct and Defaults

**Objective: Define configuration data structure with defaults.

**Tasks:
- Create `internal/config/config.go`
- Define `Config` struct with all fields
- Implement `DefaultConfig()` function
- Implement `LoadConfig()` with JSON unmarshaling
- Implement `Save()` with JSON marshaling
- Implement `Validate()` with field validation

```go
package config

import (
    "encoding/json"
    "os"
    "path/filepath"
)

type Config struct {
    ServerPath    string `json:"server_path"`
    ModelPath   string `json:"model_path"`
    Port       int     `json:"port"`
    GPULayers   int     `json:"n_gpu_layers"`
    N_CTX      int     `json:"n_ctx"`
    Threads    int     `json:"threads"`
    SystemPrompt string `json:"system_prompt"`
    Streaming  bool    `json:"streaming"`
    Theme      string `json:"theme"` // "dark" | "light"`
    FilePath   string `json:"-"`
}

func DefaultConfig() *Config {
    return &Config{
        Port: 8080,
        GPULayers: 99,
        N_CTX: 4096,
        Threads: 8,
        Streaming: true,
        Theme: "dark,
    }
}

func LoadConfig(path string) (*Config, error) {
    // Read file, unmarshal JSON, validate
}

func (c*Config) Save() error {
    // Marshal JSON, write file
}

func (c*Config) Validate() error {
    // Check server_path exists
    // Check model_path exists
    // Check port is valid
    // Check threads is valid
    // Check n_gpu_layers is valid
}
```

**Success Criteria:
- Config loads from file
- `Save()` writes valid JSON
- `Validate()` catches errors (empty paths, invalid port, etc.)

**Dependencies: Step 1.1 (module exists)

---

### Step 2.2: Config File Location

**Objective: Config file location strategy.

**Tasks:
- Use `os.Executable()` to find binary path
- Place config.json alongside the binary
- Create `GetConfigPath()` function

```go
func GetConfigPath() string {
    exe, err := os.Executable()
    if err != nil {
        return "config.json"
    }
    dir := filepath.Dir(exe)
    return filepath.Join(dir, "config.json")
}
```

**Success Criteria:
- Config file is found next to binary

**Dependencies: Step 2.1

---

### Step 2.3: Config Persistence

**Objective: Persistent settings between sessions.

**Tasks:
- Implement file read/write logic
- Handle file not found gracefully
- Return default config when file missing

**Success Criteria:
- Config file is created on first run
- Existing config is loaded on subsequent runs

**Dependencies: Step 2.1

---

## Files Created:
- `internal/config/config.go`

## Dependencies on other phases:
- Phase 1 (go.mod, module exists)
- Phase 3 (Server Manager needs config
- Phase 4 (Chat Client needs base URL from config)

## Review Notes:
- Config file uses absolute paths
- Config location is next to binary via `os.Executable()`
- Validation checks file existence for server_path, model_path
- Port validation: 1024-65535
- Threads validation: 1-64
- GPU layers validation: 0-99 (0 = CPU only, 99 = all layers to GPU)
- Theme validation: "dark", "light", "system"
