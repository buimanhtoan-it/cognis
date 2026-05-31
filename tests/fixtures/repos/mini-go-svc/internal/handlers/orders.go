// Package handlers contains the HTTP request handlers for mini-go-svc.
//
// OrdersHandler is the planted goroutine surface for cognis tests:
// CreateOrder dispatches an audit event from a fresh `go func() { ... }()`
// without error handling, which the review-mode classifier should pick
// up as a concurrency smell candidate.
package handlers

import (
	"context"
	"errors"
	"net/http"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/google/uuid"

	"mini-go-svc/internal/db"
	"mini-go-svc/internal/validation"
)

// auditRecorder is the dependency contract OrdersHandler needs from
// db.AuditSink. Stating it as an interface means the handler can be
// unit-tested without the goroutine-backed real sink.
type auditRecorder interface {
	Record(ctx context.Context, event, subjectID string)
}

// OrdersHandler binds /api/v1/orders endpoints to a repo plus an audit
// sink. It is safe to share between goroutines provided the underlying
// dependencies are.
type OrdersHandler struct {
	repo  *db.OrderRepo
	audit auditRecorder
	clock func() time.Time
}

// NewOrdersHandler wires an OrdersHandler with the given dependencies.
// Clock defaults to time.Now and can be overridden via SetClock.
func NewOrdersHandler(repo *db.OrderRepo, audit auditRecorder) *OrdersHandler {
	return &OrdersHandler{
		repo:  repo,
		audit: audit,
		clock: time.Now,
	}
}

// SetClock injects a deterministic clock for tests.
func (h *OrdersHandler) SetClock(clock func() time.Time) {
	if clock != nil {
		h.clock = clock
	}
}

// CreateOrder validates and inserts a new order, then dispatches an audit
// event from a goroutine.
//
// PLANTED: the audit goroutine has no error handling and is unbounded —
// review-mode capsules should surface this.
func (h *OrdersHandler) CreateOrder(c *gin.Context) {
	req := &validation.OrderRequest{}
	if err := c.ShouldBindJSON(req); err != nil {
		respondBadRequest(c, "invalid_json", err.Error())
		return
	}
	if req.ID == "" {
		req.ID = uuid.NewString()
	}
	if req.Status == "" {
		req.Status = "pending"
	}

	if err := validation.ValidateOrder(req); err != nil {
		respondBadRequest(c, "invalid_order", err.Error())
		return
	}

	now := h.clock().UTC()
	order := db.Order{
		ID:         req.ID,
		CustomerID: req.CustomerID,
		TotalCents: req.TotalCents,
		Status:     req.Status,
		CreatedAt:  now,
		UpdatedAt:  now,
	}
	if err := h.repo.Insert(c.Request.Context(), order); err != nil {
		respondError(c, http.StatusConflict, "duplicate_order", err.Error())
		return
	}

	// PLANTED-ISSUE: audit dispatch leaks a goroutine on every request.
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		h.audit.Record(ctx, "order.created", order.ID)
	}()

	c.JSON(http.StatusCreated, orderResponse(order))
}

// GetOrder returns a single order by id.
func (h *OrdersHandler) GetOrder(c *gin.Context) {
	id := strings.TrimSpace(c.Param("id"))
	if id == "" {
		respondBadRequest(c, "missing_id", "order id is required")
		return
	}
	order, err := h.repo.FindByID(c.Request.Context(), id)
	if errors.Is(err, db.ErrNotFound) {
		respondError(c, http.StatusNotFound, "not_found", err.Error())
		return
	}
	if err != nil {
		respondError(c, http.StatusInternalServerError, "lookup_failed", err.Error())
		return
	}
	c.JSON(http.StatusOK, orderResponse(*order))
}

// UpdateOrder mutates an existing order via PATCH.
func (h *OrdersHandler) UpdateOrder(c *gin.Context) {
	id := strings.TrimSpace(c.Param("id"))
	if id == "" {
		respondBadRequest(c, "missing_id", "order id is required")
		return
	}

	patch := &updateRequest{}
	if err := c.ShouldBindJSON(patch); err != nil {
		respondBadRequest(c, "invalid_json", err.Error())
		return
	}

	existing, err := h.repo.FindByID(c.Request.Context(), id)
	if errors.Is(err, db.ErrNotFound) {
		respondError(c, http.StatusNotFound, "not_found", err.Error())
		return
	}
	if err != nil {
		respondError(c, http.StatusInternalServerError, "lookup_failed", err.Error())
		return
	}

	total := existing.TotalCents
	if patch.TotalCents != nil {
		total = *patch.TotalCents
	}
	status := existing.Status
	if patch.Status != nil {
		status = *patch.Status
	}

	updated, err := h.repo.Update(c.Request.Context(), id, total, status)
	if err != nil {
		respondError(c, http.StatusInternalServerError, "update_failed", err.Error())
		return
	}

	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		h.audit.Record(ctx, "order.updated", updated.ID)
	}()

	c.JSON(http.StatusOK, orderResponse(*updated))
}

// CancelOrder transitions an order to status="cancelled".
func (h *OrdersHandler) CancelOrder(c *gin.Context) {
	id := strings.TrimSpace(c.Param("id"))
	if id == "" {
		respondBadRequest(c, "missing_id", "order id is required")
		return
	}
	if err := h.repo.CancelByID(c.Request.Context(), id); err != nil {
		switch {
		case errors.Is(err, db.ErrNotFound):
			respondError(c, http.StatusNotFound, "not_found", err.Error())
		case errors.Is(err, db.ErrAlreadyCancelled):
			respondError(c, http.StatusConflict, "already_cancelled", err.Error())
		default:
			respondError(c, http.StatusInternalServerError, "cancel_failed", err.Error())
		}
		return
	}

	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		h.audit.Record(ctx, "order.cancelled", id)
	}()

	c.Status(http.StatusNoContent)
}

// updateRequest is the PATCH body for UpdateOrder. Fields are pointers so
// the caller can express "leave as-is" by omitting them.
type updateRequest struct {
	TotalCents *int64  `json:"total_cents,omitempty"`
	Status     *string `json:"status,omitempty"`
}

// orderResponse renders a public-facing order dictionary.
func orderResponse(o db.Order) map[string]any {
	return map[string]any{
		"id":          o.ID,
		"customer_id": o.CustomerID,
		"total_cents": o.TotalCents,
		"status":      o.Status,
		"created_at":  o.CreatedAt.Format(time.RFC3339),
		"updated_at":  o.UpdatedAt.Format(time.RFC3339),
	}
}

// respondBadRequest writes a 400 with a structured error envelope.
func respondBadRequest(c *gin.Context, code, message string) {
	respondError(c, http.StatusBadRequest, code, message)
}

// respondError writes a status code with `{"error": {"code", "message"}}`.
func respondError(c *gin.Context, status int, code, message string) {
	c.AbortWithStatusJSON(status, gin.H{
		"error": gin.H{
			"code":    code,
			"message": message,
		},
	})
}
