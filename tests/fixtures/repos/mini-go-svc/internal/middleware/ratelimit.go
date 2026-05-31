// Package middleware contains gin middleware implementations.
//
// ratelimit.go is the planted goroutine surface: NewRateLimiter spawns a
// long-lived refill goroutine. The cognis enricher should attach
// `spawns_goroutine=true` to the symbol.
package middleware

import (
	"net/http"
	"time"

	"github.com/gin-gonic/gin"
)

// rateLimiter is the internal token-bucket type. Exposed via NewRateLimiter
// rather than directly so callers don't depend on the field layout.
type rateLimiter struct {
	tokens chan struct{}
	rate   int
	burst  int
}

// NewRateLimiter returns a gin.HandlerFunc that admits at most `rate`
// requests per second with a burst capacity of `burst`. The middleware
// owns a goroutine that periodically refills the token bucket; that
// goroutine exits when Stop is invoked on the underlying limiter.
//
// Note: the returned middleware does not currently expose a way to stop
// the refill goroutine. This is part of the planted concurrency-smell
// surface (see README §3 / §4).
func NewRateLimiter(rate, burst int) gin.HandlerFunc {
	if rate <= 0 {
		rate = 1
	}
	if burst <= rate {
		burst = rate * 2
	}

	rl := &rateLimiter{
		tokens: make(chan struct{}, burst),
		rate:   rate,
		burst:  burst,
	}

	// Pre-fill the bucket so cold starts don't reject the first request.
	for i := 0; i < burst; i++ {
		rl.tokens <- struct{}{}
	}

	go rl.refill()

	return func(c *gin.Context) {
		select {
		case <-rl.tokens:
			c.Next()
		default:
			c.AbortWithStatusJSON(http.StatusTooManyRequests, gin.H{
				"error": gin.H{
					"code":    "rate_limited",
					"message": "too many requests",
				},
			})
		}
	}
}

// refill is the long-lived goroutine that tops up the token bucket. It
// runs once per `1s / rate`, attempting a non-blocking send so a full
// bucket simply discards the would-be token.
func (r *rateLimiter) refill() {
	interval := time.Second / time.Duration(r.rate)
	if interval <= 0 {
		interval = time.Millisecond
	}
	ticker := time.NewTicker(interval)
	defer ticker.Stop()

	for range ticker.C {
		select {
		case r.tokens <- struct{}{}:
		default:
			// bucket already full — token discarded
		}
	}
}

// JWTValidator is the minimal contract NewJWTGuard depends on. Real
// callers pass *auth.Validator; tests pass a stub.
type JWTValidator interface {
	Validate(token string) (*Claims, error)
}

// Claims is a tiny mirror of auth.Claims used here to avoid an import
// cycle. Only the fields the middleware needs are surfaced.
type Claims struct {
	Subject string
	Roles   []string
	Email   string
}

// NewJWTGuard returns gin middleware that requires a Bearer token on the
// Authorization header. Failed validation returns 401.
func NewJWTGuard(validator JWTValidator) gin.HandlerFunc {
	return func(c *gin.Context) {
		header := c.GetHeader("Authorization")
		token := stripBearer(header)
		if token == "" {
			c.AbortWithStatusJSON(http.StatusUnauthorized, gin.H{
				"error": gin.H{"code": "missing_token", "message": "Authorization header required"},
			})
			return
		}
		claims, err := validator.Validate(token)
		if err != nil {
			c.AbortWithStatusJSON(http.StatusUnauthorized, gin.H{
				"error": gin.H{"code": "invalid_token", "message": err.Error()},
			})
			return
		}
		c.Set("subject", claims.Subject)
		c.Set("roles", claims.Roles)
		c.Set("email", claims.Email)
		c.Next()
	}
}

func stripBearer(header string) string {
	const prefix = "Bearer "
	if len(header) <= len(prefix) {
		return ""
	}
	if header[:len(prefix)] != prefix {
		return ""
	}
	return header[len(prefix):]
}
