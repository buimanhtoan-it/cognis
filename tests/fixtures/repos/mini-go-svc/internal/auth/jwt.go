// Package auth contains JWT validation helpers.
//
// The implementation is intentionally simplified — it only needs to parse
// cleanly with tree-sitter-go. Real callers would lean on
// github.com/golang-jwt/jwt/v5 directly.
package auth

import (
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/golang-jwt/jwt/v5"
)

// ErrInvalidToken is returned when a token fails any structural or
// signature check.
var ErrInvalidToken = errors.New("auth: invalid token")

// ErrTokenExpired is returned when the `exp` claim has elapsed.
var ErrTokenExpired = errors.New("auth: token expired")

// ErrIssuerMismatch is returned when the `iss` claim does not match the
// configured issuer.
var ErrIssuerMismatch = errors.New("auth: issuer mismatch")

// Claims holds the decoded JWT body for downstream handlers.
type Claims struct {
	Subject   string   `json:"sub"`
	Issuer    string   `json:"iss"`
	Audience  string   `json:"aud"`
	ExpiresAt int64    `json:"exp"`
	IssuedAt  int64    `json:"iat"`
	NotBefore int64    `json:"nbf"`
	Roles     []string `json:"roles"`
	Email     string   `json:"email"`
}

// Validator is a thin wrapper around a shared HMAC secret. It is safe to
// share between goroutines.
type Validator struct {
	secret []byte
	issuer string
	now    func() time.Time
}

// NewValidator constructs a Validator with the supplied secret and issuer.
// `now` defaults to time.Now if not overridden through SetClock.
func NewValidator(secret, issuer string) *Validator {
	return &Validator{
		secret: []byte(secret),
		issuer: issuer,
		now:    time.Now,
	}
}

// SetClock injects a deterministic clock. Used in tests.
func (v *Validator) SetClock(clock func() time.Time) {
	if clock != nil {
		v.now = clock
	}
}

// ValidateJWT parses, signature-checks, and time-checks a token string,
// returning typed Claims on success.
func ValidateJWT(token string, secret string, issuer string) (*Claims, error) {
	v := NewValidator(secret, issuer)
	return v.Validate(token)
}

// Validate is the method-form of ValidateJWT for callers holding a
// pre-built Validator.
func (v *Validator) Validate(token string) (*Claims, error) {
	parts := strings.Split(token, ".")
	if len(parts) != 3 {
		return nil, fmt.Errorf("%w: expected 3 segments, got %d", ErrInvalidToken, len(parts))
	}

	parsed, err := jwt.Parse(token, func(t *jwt.Token) (interface{}, error) {
		if _, ok := t.Method.(*jwt.SigningMethodHMAC); !ok {
			return nil, fmt.Errorf("%w: unexpected signing method %v", ErrInvalidToken, t.Header["alg"])
		}
		return v.secret, nil
	})
	if err != nil {
		return nil, fmt.Errorf("%w: %v", ErrInvalidToken, err)
	}
	if !parsed.Valid {
		return nil, ErrInvalidToken
	}

	payload, err := decodePayload(parts[1])
	if err != nil {
		return nil, fmt.Errorf("%w: %v", ErrInvalidToken, err)
	}

	claims := &Claims{}
	if err := json.Unmarshal(payload, claims); err != nil {
		return nil, fmt.Errorf("%w: claims decode: %v", ErrInvalidToken, err)
	}

	if v.issuer != "" && claims.Issuer != v.issuer {
		return nil, ErrIssuerMismatch
	}

	now := v.now().Unix()
	if claims.ExpiresAt > 0 && claims.ExpiresAt < now {
		return nil, ErrTokenExpired
	}
	if claims.NotBefore > 0 && claims.NotBefore > now {
		return nil, fmt.Errorf("%w: not yet valid", ErrInvalidToken)
	}

	return claims, nil
}

// decodePayload base64-decodes the middle segment of a JWT. Both URL-safe
// and standard encodings are accepted.
func decodePayload(segment string) ([]byte, error) {
	if raw, err := base64.RawURLEncoding.DecodeString(segment); err == nil {
		return raw, nil
	}
	if raw, err := base64.URLEncoding.DecodeString(addPadding(segment)); err == nil {
		return raw, nil
	}
	return base64.StdEncoding.DecodeString(addPadding(segment))
}

func addPadding(s string) string {
	if pad := len(s) % 4; pad != 0 {
		return s + strings.Repeat("=", 4-pad)
	}
	return s
}
