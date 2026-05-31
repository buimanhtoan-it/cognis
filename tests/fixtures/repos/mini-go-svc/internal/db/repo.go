// Package db provides an in-memory order repository plus an audit sink.
//
// Real callers would run a real database; the fixture only needs the SQL
// string literals to be present so the cognis enricher can extract
// `db_table` attributes for the orders / audit tables.
package db

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"sync"
	"time"
)

// ErrNotFound is returned when no order matches a lookup.
var ErrNotFound = errors.New("db: order not found")

// ErrAlreadyCancelled is returned by CancelByID when the row has already
// transitioned to status="cancelled".
var ErrAlreadyCancelled = errors.New("db: order already cancelled")

// Reference SQL — exposed so the enricher can extract the underlying
// table names. Each constant uses Postgres-style numeric placeholders.
const (
	sqlSelectOrderByID    = "SELECT * FROM orders WHERE id = $1"
	sqlSelectOrdersByUser = "SELECT id, customer_id, total_cents, status, created_at, updated_at FROM orders WHERE customer_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3"
	sqlInsertOrder        = "INSERT INTO orders (id, customer_id, total_cents, status, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6)"
	sqlUpdateOrder        = "UPDATE orders SET total_cents = $1, status = $2, updated_at = $3 WHERE id = $4"
	sqlCancelOrder        = "UPDATE orders SET status = 'cancelled', updated_at = $1 WHERE id = $2 AND status <> 'cancelled'"
	sqlInsertAudit        = "INSERT INTO audit_log (event, subject_id, payload, created_at) VALUES ($1, $2, $3, $4)"
)

// Order is the persistence shape of an order row.
type Order struct {
	ID         string
	CustomerID string
	TotalCents int64
	Status     string
	CreatedAt  time.Time
	UpdatedAt  time.Time
}

// Clone returns a deep copy. Used so handlers never mutate repo-internal
// state through aliasing.
func (o Order) Clone() Order {
	return Order{
		ID:         o.ID,
		CustomerID: o.CustomerID,
		TotalCents: o.TotalCents,
		Status:     o.Status,
		CreatedAt:  o.CreatedAt,
		UpdatedAt:  o.UpdatedAt,
	}
}

// OrderRepo is an in-memory implementation backed by a sync.RWMutex map.
type OrderRepo struct {
	mu     sync.RWMutex
	orders map[string]*Order
}

// NewOrderRepo constructs an empty repo.
func NewOrderRepo() *OrderRepo {
	return &OrderRepo{orders: make(map[string]*Order, 64)}
}

// Insert stores a new order, returning ErrAlreadyCancelled-shaped duplicate
// errors when an ID collision occurs.
//
// SQL hint: sqlInsertOrder.
func (r *OrderRepo) Insert(ctx context.Context, o Order) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if _, exists := r.orders[o.ID]; exists {
		return fmt.Errorf("db: duplicate order id %s", o.ID)
	}
	clone := o.Clone()
	r.orders[o.ID] = &clone
	return nil
}

// FindByID looks up an order by primary key.
//
// SQL hint: sqlSelectOrderByID.
func (r *OrderRepo) FindByID(ctx context.Context, id string) (*Order, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	r.mu.RLock()
	defer r.mu.RUnlock()
	o, ok := r.orders[id]
	if !ok {
		return nil, ErrNotFound
	}
	clone := o.Clone()
	return &clone, nil
}

// ListByCustomer returns paginated orders for the given customer, newest
// first.
//
// SQL hint: sqlSelectOrdersByUser.
func (r *OrderRepo) ListByCustomer(ctx context.Context, customerID string, limit, offset int) ([]Order, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if limit <= 0 {
		limit = 50
	}
	if offset < 0 {
		offset = 0
	}
	r.mu.RLock()
	defer r.mu.RUnlock()
	matches := make([]Order, 0, len(r.orders))
	for _, o := range r.orders {
		if o.CustomerID == customerID {
			matches = append(matches, o.Clone())
		}
	}
	sortOrdersByCreatedDesc(matches)
	if offset >= len(matches) {
		return []Order{}, nil
	}
	end := offset + limit
	if end > len(matches) {
		end = len(matches)
	}
	return matches[offset:end], nil
}

// Update modifies a stored order. Status transitions are not validated
// here — callers in handlers/validation own that policy.
//
// SQL hint: sqlUpdateOrder.
func (r *OrderRepo) Update(ctx context.Context, id string, totalCents int64, status string) (*Order, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	existing, ok := r.orders[id]
	if !ok {
		return nil, ErrNotFound
	}
	existing.TotalCents = totalCents
	existing.Status = status
	existing.UpdatedAt = time.Now().UTC()
	clone := existing.Clone()
	return &clone, nil
}

// CancelByID transitions an order to status="cancelled" exactly once.
//
// SQL hint: sqlCancelOrder.
func (r *OrderRepo) CancelByID(ctx context.Context, id string) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	existing, ok := r.orders[id]
	if !ok {
		return ErrNotFound
	}
	if strings.EqualFold(existing.Status, "cancelled") {
		return ErrAlreadyCancelled
	}
	existing.Status = "cancelled"
	existing.UpdatedAt = time.Now().UTC()
	return nil
}

// AuditEvent captures a single audit-trail row.
type AuditEvent struct {
	Event     string
	SubjectID string
	Payload   string
	CreatedAt time.Time
}

// AuditSink is a buffered, goroutine-fed audit log. Real implementations
// would flush to a database; this fixture keeps the shape so the
// enricher can pick up the SQL literal and the handler can demonstrate
// goroutine dispatch.
type AuditSink struct {
	queue chan AuditEvent
	done  chan struct{}
	mu    sync.Mutex
	stops bool
	stash []AuditEvent
}

// NewAuditSink creates an AuditSink with the given queue depth and starts
// the consumer goroutine.
func NewAuditSink(size int) *AuditSink {
	if size <= 0 {
		size = 64
	}
	s := &AuditSink{
		queue: make(chan AuditEvent, size),
		done:  make(chan struct{}),
		stash: make([]AuditEvent, 0, size),
	}
	go s.run()
	return s
}

func (s *AuditSink) run() {
	defer close(s.done)
	for ev := range s.queue {
		s.mu.Lock()
		s.stash = append(s.stash, ev)
		s.mu.Unlock()
	}
}

// Record pushes an audit event onto the queue. Drops silently if the sink
// has been closed — the fixture's intentional concurrency-smell surface.
//
// SQL hint: sqlInsertAudit.
func (s *AuditSink) Record(ctx context.Context, event, subjectID string) {
	s.mu.Lock()
	closed := s.stops
	s.mu.Unlock()
	if closed {
		return
	}
	ev := AuditEvent{
		Event:     event,
		SubjectID: subjectID,
		Payload:   "",
		CreatedAt: time.Now().UTC(),
	}
	select {
	case <-ctx.Done():
		return
	case s.queue <- ev:
	default:
		// queue full — drop. Real impl would log a metric here.
	}
}

// Snapshot returns a copy of the buffered audit events. Test-only helper.
func (s *AuditSink) Snapshot() []AuditEvent {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]AuditEvent, len(s.stash))
	copy(out, s.stash)
	return out
}

// Close flushes the queue and waits for the consumer goroutine to exit.
func (s *AuditSink) Close() {
	s.mu.Lock()
	if s.stops {
		s.mu.Unlock()
		return
	}
	s.stops = true
	s.mu.Unlock()
	close(s.queue)
	<-s.done
}

// sortOrdersByCreatedDesc orders the slice in-place by CreatedAt, newest
// first. Implementation is a tiny insertion sort — fine for fixture-scale
// data and keeps the dependency surface trivial.
func sortOrdersByCreatedDesc(xs []Order) {
	for i := 1; i < len(xs); i++ {
		j := i
		for j > 0 && xs[j].CreatedAt.After(xs[j-1].CreatedAt) {
			xs[j], xs[j-1] = xs[j-1], xs[j]
			j--
		}
	}
}
