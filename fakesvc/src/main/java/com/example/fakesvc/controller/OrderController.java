package com.example.fakesvc.controller;

import com.example.fakesvc.model.Order;
import io.swagger.v3.oas.annotations.Operation;
import io.swagger.v3.oas.annotations.tags.Tag;
import org.springframework.http.HttpStatus;
import org.springframework.http.ResponseEntity;
import org.springframework.web.bind.annotation.*;
import jakarta.annotation.PostConstruct;

import java.util.Collection;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicLong;
import java.util.stream.Collectors;

@RestController
@RequestMapping("/api/orders")
@Tag(name = "Orders", description = "订单 CRUD（需 JWT 认证）")
public class OrderController {

    private final Map<Long, Order> store = new ConcurrentHashMap<>();
    private final AtomicLong idGen = new AtomicLong(1);

    @PostConstruct
    public void initSampleData() {
        store.put(1L, new Order(1L, "admin", "Laptop", 9999.00, "shipped"));
        store.put(2L, new Order(2L, "admin", "Mouse", 199.00, "delivered"));
        store.put(3L, new Order(3L, "alice", "Keyboard", 599.00, "processing"));
    }

    @GetMapping
    @Operation(summary = "获取所有订单（可选按 userId 过滤）")
    public Collection<Order> list(@RequestParam(required = false) String userId) {
        if (userId != null) {
            return store.values().stream()
                .filter(o -> o.getUserId().equals(userId))
                .collect(Collectors.toList());
        }
        return store.values();
    }

    @GetMapping("/{id}")
    @Operation(summary = "获取单个订单")
    public ResponseEntity<Order> get(@PathVariable Long id) {
        Order order = store.get(id);
        return order != null ? ResponseEntity.ok(order) : ResponseEntity.notFound().build();
    }

    @PostMapping
    @Operation(summary = "创建订单")
    public ResponseEntity<Order> create(@RequestBody Order input) {
        long id = idGen.getAndIncrement();
        Order order = new Order(id, input.getUserId(), input.getProduct(), input.getAmount(), input.getStatus());
        store.put(id, order);
        return ResponseEntity.status(HttpStatus.CREATED).body(order);
    }

    @DeleteMapping("/{id}")
    @Operation(summary = "删除订单")
    public ResponseEntity<Void> delete(@PathVariable Long id) {
        return store.remove(id) != null
            ? ResponseEntity.noContent().build()
            : ResponseEntity.notFound().build();
    }
}
