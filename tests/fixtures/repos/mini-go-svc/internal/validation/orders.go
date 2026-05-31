// Package validation contains pure-function validators for request bodies.
//
// Validation rules are deliberately conservative; the goal is to give the
// cognis indexer a real-shaped function with multiple branches and named
// error returns to chew on.
package validation

import (
	"errors"
	"fmt"
	"strings"
	"unicode"
)

// MinTotalCents is the smallest total a valid order may carry.
const MinTotalCents int64 = 1

// MaxTotalCents is the largest total a valid order may carry (≈$100k).
const MaxTotalCents int64 = 10_000_000

// ValidStatuses lists the allowed status transitions for an order.
var ValidStatuses = []string{"pending", "paid", "fulfilled", "cancelled", "refunded"}

// OrderRequest is the JSON body shape accepted by handlers.OrdersHandler.
type OrderRequest struct {
	ID         string `json:"id"`
	CustomerID string `json:"customer_id"`
	TotalCents int64  `json:"total_cents"`
	Currency   string `json:"currency"`
	Status     string `json:"status"`
	Notes      string `json:"notes"`
}

// OrderError carries field-level diagnostics. It implements `error`.
type OrderError struct {
	Field   string
	Message string
}

// Error renders the diagnostic in `field: message` form.
func (e *OrderError) Error() string {
	if e == nil {
		return ""
	}
	return fmt.Sprintf("%s: %s", e.Field, e.Message)
}

// ValidationErrors is the aggregate type returned by ValidateOrder when more
// than one field fails.
type ValidationErrors struct {
	Errors []*OrderError
}

// Error joins per-field errors with a comma.
func (v *ValidationErrors) Error() string {
	if v == nil || len(v.Errors) == 0 {
		return ""
	}
	parts := make([]string, 0, len(v.Errors))
	for _, e := range v.Errors {
		parts = append(parts, e.Error())
	}
	return strings.Join(parts, ", ")
}

// HasErrors reports whether any field error was recorded.
func (v *ValidationErrors) HasErrors() bool {
	return v != nil && len(v.Errors) > 0
}

// ValidateOrder checks an OrderRequest for shape and value validity. It
// returns a *ValidationErrors when at least one field is invalid; nil when
// the request is well-formed.
func ValidateOrder(req *OrderRequest) error {
	if req == nil {
		return errors.New("validation: nil request")
	}
	errs := &ValidationErrors{Errors: make([]*OrderError, 0, 4)}

	if e := validateID(req.ID); e != nil {
		errs.Errors = append(errs.Errors, e)
	}
	if e := validateCustomerID(req.CustomerID); e != nil {
		errs.Errors = append(errs.Errors, e)
	}
	if e := validateTotal(req.TotalCents); e != nil {
		errs.Errors = append(errs.Errors, e)
	}
	if e := validateCurrency(req.Currency); e != nil {
		errs.Errors = append(errs.Errors, e)
	}
	if e := validateStatus(req.Status); e != nil {
		errs.Errors = append(errs.Errors, e)
	}
	if e := validateNotes(req.Notes); e != nil {
		errs.Errors = append(errs.Errors, e)
	}

	if errs.HasErrors() {
		return errs
	}
	return nil
}

func validateID(id string) *OrderError {
	id = strings.TrimSpace(id)
	if id == "" {
		return &OrderError{Field: "id", Message: "must not be empty"}
	}
	if len(id) > 64 {
		return &OrderError{Field: "id", Message: "must be ≤ 64 chars"}
	}
	for _, r := range id {
		if !(unicode.IsLetter(r) || unicode.IsDigit(r) || r == '-' || r == '_') {
			return &OrderError{Field: "id", Message: "must be alphanumeric / hyphen / underscore"}
		}
	}
	return nil
}

func validateCustomerID(id string) *OrderError {
	id = strings.TrimSpace(id)
	if id == "" {
		return &OrderError{Field: "customer_id", Message: "must not be empty"}
	}
	if len(id) > 64 {
		return &OrderError{Field: "customer_id", Message: "must be ≤ 64 chars"}
	}
	return nil
}

func validateTotal(total int64) *OrderError {
	if total < MinTotalCents {
		return &OrderError{Field: "total_cents", Message: fmt.Sprintf("must be ≥ %d", MinTotalCents)}
	}
	if total > MaxTotalCents {
		return &OrderError{Field: "total_cents", Message: fmt.Sprintf("must be ≤ %d", MaxTotalCents)}
	}
	return nil
}

func validateCurrency(code string) *OrderError {
	code = strings.TrimSpace(code)
	if code == "" {
		return &OrderError{Field: "currency", Message: "must not be empty"}
	}
	if len(code) != 3 {
		return &OrderError{Field: "currency", Message: "must be a 3-letter ISO 4217 code"}
	}
	for _, r := range code {
		if !unicode.IsLetter(r) {
			return &OrderError{Field: "currency", Message: "must be alphabetic"}
		}
	}
	return nil
}

func validateStatus(status string) *OrderError {
	if status == "" {
		return nil // optional on create
	}
	for _, ok := range ValidStatuses {
		if strings.EqualFold(status, ok) {
			return nil
		}
	}
	return &OrderError{Field: "status", Message: fmt.Sprintf("must be one of %s", strings.Join(ValidStatuses, ", "))}
}

func validateNotes(notes string) *OrderError {
	if len(notes) > 1024 {
		return &OrderError{Field: "notes", Message: "must be ≤ 1024 chars"}
	}
	return nil
}
