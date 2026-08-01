package com.example.fakesvc.controller;

import com.example.fakesvc.model.User;
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

/**
 * 用户 CRUD — 内存存储，需要 Bearer token。
 */
@RestController
@RequestMapping("/api/users")
@Tag(name = "Users", description = "用户 CRUD（需 JWT 认证）")
public class UserController {

    private final Map<Long, User> store = new ConcurrentHashMap<>();
    private final AtomicLong idGen = new AtomicLong(1);

    @PostConstruct
    public void initSampleData() {
        store.put(1L, new User(1L, "Alice", "alice@example.com"));
        store.put(2L, new User(2L, "admin", "admin@example.com"));
    }

    @GetMapping
    @Operation(summary = "获取所有用户")
    public Collection<User> list() {
        return store.values();
    }

    @GetMapping("/{id}")
    @Operation(summary = "获取单个用户（按数字 ID）")
    public ResponseEntity<User> get(@PathVariable Long id) {
        User user = store.get(id);
        return user != null ? ResponseEntity.ok(user) : ResponseEntity.notFound().build();
    }

    @GetMapping("/by-name/{name}")
    @Operation(summary = "按名称查询用户（支持 OIDC sub 等字符串标识）")
    public ResponseEntity<User> getByName(@PathVariable String name) {
        User user = store.values().stream()
            .filter(u -> u.getName().equals(name))
            .findFirst()
            .orElse(null);
        return user != null ? ResponseEntity.ok(user) : ResponseEntity.notFound().build();
    }

    @PostMapping
    @Operation(summary = "创建用户")
    public ResponseEntity<User> create(@RequestBody User input) {
        long id = idGen.getAndIncrement();
        User user = new User(id, input.getName(), input.getEmail());
        store.put(id, user);
        return ResponseEntity.status(HttpStatus.CREATED).body(user);
    }

    @PutMapping("/{id}")
    @Operation(summary = "更新用户")
    public ResponseEntity<User> update(@PathVariable Long id, @RequestBody User input) {
        User existing = store.get(id);
        if (existing == null) {
            return ResponseEntity.notFound().build();
        }
        existing.setName(input.getName());
        existing.setEmail(input.getEmail());
        return ResponseEntity.ok(existing);
    }

    @DeleteMapping("/{id}")
    @Operation(summary = "删除用户")
    public ResponseEntity<Void> delete(@PathVariable Long id) {
        return store.remove(id) != null
            ? ResponseEntity.noContent().build()
            : ResponseEntity.notFound().build();
    }
}
