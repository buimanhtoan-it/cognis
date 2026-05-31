// Package main is the entry point for the mini-go-svc fixture binary.
//
// This file deliberately carries an unused-import (see the import block
// below). It is part of the cognis fixture set; the cognis review-mode
// classifier should surface the dead import.
package main

import (
	"context"
	"errors"
	"fmt" // PLANTED-ISSUE: dead-import — referenced nowhere; kept for review-mode classifier coverage
	"log"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/gin-gonic/gin"

	"mini-go-svc/internal/auth"
	"mini-go-svc/internal/config"
	"mini-go-svc/internal/db"
	"mini-go-svc/internal/handlers"
	"mini-go-svc/internal/middleware"
)

// shutdownTimeout bounds the graceful shutdown window.
const shutdownTimeout = 10 * time.Second

// setupRouter wires every middleware, route group, and handler onto a fresh
// gin.Engine. LegacyHandler is intentionally NOT registered here — the
// orphaned export is part of the planted issue surface (see README.md).
func setupRouter(cfg *config.Config, repo *db.OrderRepo, auditor *db.AuditSink) *gin.Engine {
	gin.SetMode(gin.ReleaseMode)
	router := gin.New()

	router.Use(gin.Recovery())
	router.Use(middleware.NewRequestLogger(cfg.LogLevel))
	router.Use(middleware.NewAccessLog())
	router.Use(middleware.NewRateLimiter(cfg.RateLimitPerSecond, cfg.RateLimitBurst))

	router.GET("/healthz", func(c *gin.Context) {
		c.JSON(http.StatusOK, gin.H{"status": "ok", "service": cfg.ServiceName})
	})

	router.GET("/version", func(c *gin.Context) {
		c.JSON(http.StatusOK, gin.H{
			"service": cfg.ServiceName,
			"version": cfg.Version,
		})
	})

	api := router.Group("/api/v1")
	api.Use(middleware.NewJWTGuard(auth.NewValidator(cfg.JWTSecret, cfg.JWTIssuer)))

	orders := handlers.NewOrdersHandler(repo, auditor)
	api.POST("/orders", orders.CreateOrder)
	api.GET("/orders/:id", orders.GetOrder)
	api.PATCH("/orders/:id", orders.UpdateOrder)
	api.DELETE("/orders/:id", orders.CancelOrder)

	return router
}

// main starts the HTTP server, wiring config from env vars and shutting down
// gracefully on SIGINT / SIGTERM.
func main() {
	cfg, err := config.Load(os.Getenv)
	if err != nil {
		log.Fatalf("config: %v", err)
	}

	repo := db.NewOrderRepo()
	auditor := db.NewAuditSink(cfg.AuditQueueSize)

	router := setupRouter(cfg, repo, auditor)

	srv := &http.Server{
		Addr:              cfg.HTTPAddr(),
		Handler:           router,
		ReadHeaderTimeout: 5 * time.Second,
		ReadTimeout:       cfg.ReadTimeout,
		WriteTimeout:      cfg.WriteTimeout,
		IdleTimeout:       cfg.IdleTimeout,
	}

	go func() {
		log.Printf("listening on %s", srv.Addr)
		if err := srv.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
			log.Fatalf("http server: %v", err)
		}
	}()

	stop := make(chan os.Signal, 1)
	signal.Notify(stop, syscall.SIGINT, syscall.SIGTERM)
	sig := <-stop
	log.Printf("shutdown signal: %v", sig)

	ctx, cancel := context.WithTimeout(context.Background(), shutdownTimeout)
	defer cancel()

	if err := srv.Shutdown(ctx); err != nil {
		log.Printf("graceful shutdown failed: %v", err)
	}
	auditor.Close()
	log.Printf("bye")
}
