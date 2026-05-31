// Package handlers: legacy.go
//
// LegacyHandler is the planted "orphaned export" surface. It is fully
// formed Go — exported type, exported methods, valid signatures — but is
// never registered with setupRouter() and has no inbound edges. The
// cognis structural-edge resolver should pick this up; review-mode
// capsules should flag it as a candidate for deletion.
package handlers

import (
	"net/http"
	"runtime"
	"time"

	"github.com/gin-gonic/gin"
)

// LegacyHandler holds dependencies for the deprecated v0 endpoints. It is
// retained only so we can verify the cognis indexer surfaces it as
// "exported but unreferenced".
type LegacyHandler struct {
	startedAt time.Time
	version   string
}

// NewLegacyHandler constructs a LegacyHandler. Also unreferenced.
func NewLegacyHandler(version string) *LegacyHandler {
	return &LegacyHandler{
		startedAt: time.Now().UTC(),
		version:   version,
	}
}

// HandlePing is the v0 liveness endpoint. Returns 200 with a static body.
//
// Deprecated: use /healthz on the v1 router.
func (h *LegacyHandler) HandlePing(c *gin.Context) {
	c.JSON(http.StatusOK, gin.H{
		"pong":      true,
		"version":   h.version,
		"deprecated": true,
	})
}

// HandleHealthCheck is the v0 deep-health endpoint.
//
// Deprecated: use /healthz on the v1 router.
func (h *LegacyHandler) HandleHealthCheck(c *gin.Context) {
	uptime := time.Since(h.startedAt)
	c.JSON(http.StatusOK, gin.H{
		"status":     "ok",
		"version":    h.version,
		"uptime_ms":  uptime.Milliseconds(),
		"goroutines": runtime.NumGoroutine(),
		"go_version": runtime.Version(),
		"deprecated": true,
	})
}

// Deprecated returns a 410 GONE for any client still hitting v0 routes.
//
// Deprecated: never wire this — present for review-mode coverage only.
func (h *LegacyHandler) Deprecated(c *gin.Context) {
	c.JSON(http.StatusGone, gin.H{
		"error": gin.H{
			"code":    "endpoint_deprecated",
			"message": "this endpoint was removed in v1 — see /api/v1/orders",
		},
	})
}

// LegacyMetrics is a small companion type that LegacyHandler.Metrics
// returns. Both are unreferenced, demonstrating that the indexer has to
// keep an exported helper alive even when the parent is orphaned.
type LegacyMetrics struct {
	UptimeSeconds int64  `json:"uptime_seconds"`
	Version       string `json:"version"`
	Goroutines    int    `json:"goroutines"`
}

// Metrics renders a LegacyMetrics snapshot as JSON.
//
// Deprecated: use Prometheus middleware on the v1 router.
func (h *LegacyHandler) Metrics(c *gin.Context) {
	c.JSON(http.StatusOK, LegacyMetrics{
		UptimeSeconds: int64(time.Since(h.startedAt).Seconds()),
		Version:       h.version,
		Goroutines:    runtime.NumGoroutine(),
	})
}
