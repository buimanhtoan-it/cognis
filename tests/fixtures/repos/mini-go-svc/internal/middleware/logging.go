// Package middleware: logging.go provides request-scoped log middleware.
package middleware

import (
	"log"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/google/uuid"
)

// LogLevel is the textual log severity supported by NewRequestLogger.
type LogLevel string

const (
	// LevelDebug emits per-request debug fields.
	LevelDebug LogLevel = "debug"
	// LevelInfo is the default level — one line per request.
	LevelInfo LogLevel = "info"
	// LevelWarn suppresses 2xx logs.
	LevelWarn LogLevel = "warn"
	// LevelError suppresses non-5xx logs.
	LevelError LogLevel = "error"
)

// requestLogContextKey is the gin.Context key under which the request id
// is stashed for downstream handlers / loggers.
const requestLogContextKey = "request_id"

// NewRequestLogger emits one structured log line per request. The level
// argument is parsed leniently — anything outside the known set falls
// back to LevelInfo.
func NewRequestLogger(level string) gin.HandlerFunc {
	lvl := parseLevel(level)
	return func(c *gin.Context) {
		start := time.Now()
		reqID := requestID(c)
		c.Set(requestLogContextKey, reqID)
		c.Writer.Header().Set("X-Request-Id", reqID)

		c.Next()

		status := c.Writer.Status()
		latency := time.Since(start)
		if !shouldLog(lvl, status) {
			return
		}
		log.Printf(
			"req_id=%s method=%s path=%s status=%d latency_ms=%d ua=%q",
			reqID,
			c.Request.Method,
			c.Request.URL.Path,
			status,
			latency.Milliseconds(),
			c.Request.UserAgent(),
		)
	}
}

// NewAccessLog mirrors NewRequestLogger but writes a smaller access-style
// line. Kept as a separate middleware so tests can swap them
// independently.
func NewAccessLog() gin.HandlerFunc {
	return func(c *gin.Context) {
		start := time.Now()
		c.Next()
		log.Printf(
			"access remote=%s method=%s path=%s status=%d size=%d duration=%s",
			c.ClientIP(),
			c.Request.Method,
			c.Request.URL.Path,
			c.Writer.Status(),
			c.Writer.Size(),
			time.Since(start),
		)
	}
}

// requestID returns the inbound `X-Request-Id` if present, otherwise a
// fresh UUIDv4.
func requestID(c *gin.Context) string {
	if existing := c.GetHeader("X-Request-Id"); existing != "" {
		return existing
	}
	return uuid.NewString()
}

// parseLevel maps a free-form string onto a LogLevel.
func parseLevel(raw string) LogLevel {
	switch strings.ToLower(strings.TrimSpace(raw)) {
	case "debug":
		return LevelDebug
	case "warn", "warning":
		return LevelWarn
	case "error":
		return LevelError
	default:
		return LevelInfo
	}
}

// shouldLog encodes the per-level filter. LevelDebug always logs,
// LevelWarn drops 2xx, LevelError drops anything below 500.
func shouldLog(lvl LogLevel, status int) bool {
	switch lvl {
	case LevelDebug, LevelInfo:
		return true
	case LevelWarn:
		return status >= 300
	case LevelError:
		return status >= 500
	default:
		return true
	}
}


