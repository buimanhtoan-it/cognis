// Package config loads runtime configuration for mini-go-svc.
//
// Configuration is sourced exclusively from environment variables so the
// fixture exposes a tidy set of `env_var` symbols for the cognis enricher
// to recognise.
package config

import (
	"errors"
	"fmt"
	"net"
	"strconv"
	"strings"
	"time"
)

// Getter abstracts os.Getenv so tests can pass a stub map.
type Getter func(string) string

// Config is the typed runtime configuration block.
type Config struct {
	ServiceName        string
	Version            string
	Environment        string
	LogLevel           string
	Host               string
	Port               int
	ReadTimeout        time.Duration
	WriteTimeout       time.Duration
	IdleTimeout        time.Duration
	JWTSecret          string
	JWTIssuer          string
	JWTAudience        string
	JWTAccessTTL       time.Duration
	DatabaseURL        string
	DatabasePoolSize   int
	RateLimitPerSecond int
	RateLimitBurst     int
	AuditQueueSize     int
	FeatureFlags       map[string]bool
}

// HTTPAddr renders Host:Port as expected by net/http.
func (c *Config) HTTPAddr() string {
	return net.JoinHostPort(c.Host, strconv.Itoa(c.Port))
}

// IsProd is a small convenience for hot-path checks.
func (c *Config) IsProd() bool {
	return strings.EqualFold(c.Environment, "production")
}

// Load builds a *Config from the supplied Getter (typically os.Getenv).
//
// Unknown vars fall back to documented defaults; malformed values yield a
// wrapped error rather than panicking so callers can decide policy.
func Load(get Getter) (*Config, error) {
	if get == nil {
		return nil, errors.New("config: nil env getter")
	}

	port, err := parseInt(get("HTTP_PORT"), 8080)
	if err != nil {
		return nil, fmt.Errorf("HTTP_PORT: %w", err)
	}

	readTimeout, err := parseDuration(get("HTTP_READ_TIMEOUT"), 15*time.Second)
	if err != nil {
		return nil, fmt.Errorf("HTTP_READ_TIMEOUT: %w", err)
	}
	writeTimeout, err := parseDuration(get("HTTP_WRITE_TIMEOUT"), 15*time.Second)
	if err != nil {
		return nil, fmt.Errorf("HTTP_WRITE_TIMEOUT: %w", err)
	}
	idleTimeout, err := parseDuration(get("HTTP_IDLE_TIMEOUT"), 60*time.Second)
	if err != nil {
		return nil, fmt.Errorf("HTTP_IDLE_TIMEOUT: %w", err)
	}

	jwtTTL, err := parseDuration(get("JWT_ACCESS_TTL"), 15*time.Minute)
	if err != nil {
		return nil, fmt.Errorf("JWT_ACCESS_TTL: %w", err)
	}

	pool, err := parseInt(get("DATABASE_POOL_SIZE"), 10)
	if err != nil {
		return nil, fmt.Errorf("DATABASE_POOL_SIZE: %w", err)
	}

	rps, err := parseInt(get("RATE_LIMIT_PER_SECOND"), 50)
	if err != nil {
		return nil, fmt.Errorf("RATE_LIMIT_PER_SECOND: %w", err)
	}
	burst, err := parseInt(get("RATE_LIMIT_BURST"), 100)
	if err != nil {
		return nil, fmt.Errorf("RATE_LIMIT_BURST: %w", err)
	}

	auditSize, err := parseInt(get("AUDIT_QUEUE_SIZE"), 256)
	if err != nil {
		return nil, fmt.Errorf("AUDIT_QUEUE_SIZE: %w", err)
	}

	cfg := &Config{
		ServiceName:        getOrDefault(get, "SERVICE_NAME", "mini-go-svc"),
		Version:            getOrDefault(get, "SERVICE_VERSION", "0.1.0"),
		Environment:        getOrDefault(get, "ENVIRONMENT", "development"),
		LogLevel:           getOrDefault(get, "LOG_LEVEL", "info"),
		Host:               getOrDefault(get, "HTTP_HOST", "0.0.0.0"),
		Port:               port,
		ReadTimeout:        readTimeout,
		WriteTimeout:       writeTimeout,
		IdleTimeout:        idleTimeout,
		JWTSecret:          getOrDefault(get, "JWT_SECRET", "REDACT_ME_PLACEHOLDER_jwt_dev_only"),
		JWTIssuer:          getOrDefault(get, "JWT_ISSUER", "mini-go-svc"),
		JWTAudience:        getOrDefault(get, "JWT_AUDIENCE", "mini-go-svc-clients"),
		JWTAccessTTL:       jwtTTL,
		DatabaseURL:        getOrDefault(get, "DATABASE_URL", "postgres://app@localhost:5432/app?sslmode=disable"),
		DatabasePoolSize:   pool,
		RateLimitPerSecond: rps,
		RateLimitBurst:     burst,
		AuditQueueSize:     auditSize,
		FeatureFlags:       parseFlags(get),
	}

	if err := cfg.validate(); err != nil {
		return nil, err
	}
	return cfg, nil
}

func (c *Config) validate() error {
	if c.Port <= 0 || c.Port > 65535 {
		return fmt.Errorf("config: invalid port %d", c.Port)
	}
	if c.RateLimitPerSecond <= 0 {
		return fmt.Errorf("config: rate-limit must be positive, got %d", c.RateLimitPerSecond)
	}
	if c.RateLimitBurst < c.RateLimitPerSecond {
		return fmt.Errorf("config: burst (%d) must be >= rate (%d)", c.RateLimitBurst, c.RateLimitPerSecond)
	}
	if c.AuditQueueSize <= 0 {
		return fmt.Errorf("config: audit queue size must be positive, got %d", c.AuditQueueSize)
	}
	return nil
}

func getOrDefault(get Getter, key, fallback string) string {
	v := strings.TrimSpace(get(key))
	if v == "" {
		return fallback
	}
	return v
}

func parseInt(raw string, fallback int) (int, error) {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return fallback, nil
	}
	n, err := strconv.Atoi(raw)
	if err != nil {
		return 0, err
	}
	return n, nil
}

func parseDuration(raw string, fallback time.Duration) (time.Duration, error) {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return fallback, nil
	}
	return time.ParseDuration(raw)
}

func parseFlags(get Getter) map[string]bool {
	flags := make(map[string]bool, 4)
	for _, name := range []string{"FEATURE_AUDIT_ASYNC", "FEATURE_LEGACY_ROUTES", "FEATURE_STRICT_VALIDATION"} {
		v := strings.ToLower(strings.TrimSpace(get(name)))
		flags[strings.ToLower(strings.TrimPrefix(name, "FEATURE_"))] =
			v == "1" || v == "true" || v == "yes" || v == "on"
	}
	return flags
}
