package com.example.fakesvc.controller;

import io.swagger.v3.oas.annotations.Operation;
import io.swagger.v3.oas.annotations.tags.Tag;
import org.springframework.http.MediaType;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RestController;
import org.springframework.web.servlet.mvc.method.annotation.SseEmitter;

import java.time.LocalTime;
import java.time.format.DateTimeFormatter;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.TimeUnit;

/**
 * SSE 端点 — 流式推送测试。
 */
@RestController
@RequestMapping("/sse")
@Tag(name = "SSE", description = "Server-Sent Events 测试端点")
public class SseController {

    private static final DateTimeFormatter TF = DateTimeFormatter.ofPattern("HH:mm:ss");

    /**
     * 每秒推送当前时间，永不停止。
     */
    @GetMapping(path = "/clock", produces = MediaType.TEXT_EVENT_STREAM_VALUE)
    @Operation(summary = "每秒推送当前时间")
    public SseEmitter clock() {
        SseEmitter emitter = new SseEmitter(Long.MAX_VALUE);
        ScheduledExecutorService scheduler = Executors.newSingleThreadScheduledExecutor();

        scheduler.scheduleAtFixedRate(() -> {
            try {
                String time = LocalTime.now().format(TF);
                emitter.send(SseEmitter.event()
                    .data("{\"time\":\"" + time + "\"}")
                    .build());
            } catch (Exception e) {
                emitter.completeWithError(e);
                scheduler.shutdown();
            }
        }, 0, 1, TimeUnit.SECONDS);

        emitter.onCompletion(scheduler::shutdown);
        emitter.onTimeout(scheduler::shutdown);
        emitter.onError(e -> scheduler.shutdown());

        return emitter;
    }

    /**
     * 推送 10 条事件后正常关闭。
     */
    @GetMapping(path = "/events", produces = MediaType.TEXT_EVENT_STREAM_VALUE)
    @Operation(summary = "推送 10 条事件后关闭连接")
    public SseEmitter events() {
        SseEmitter emitter = new SseEmitter();
        ScheduledExecutorService scheduler = Executors.newSingleThreadScheduledExecutor();

        scheduler.scheduleAtFixedRate(new Runnable() {
            int count = 0;

            @Override
            public void run() {
                try {
                    if (count >= 10) {
                        emitter.complete();
                        scheduler.shutdown();
                        return;
                    }
                    count++;
                    emitter.send(SseEmitter.event()
                        .id(String.valueOf(count))
                        .name("tick")
                        .data("{\"seq\":" + count + ",\"time\":\"" + LocalTime.now().format(TF) + "\"}")
                        .build());
                } catch (Exception e) {
                    emitter.completeWithError(e);
                    scheduler.shutdown();
                }
            }
        }, 0, 500, TimeUnit.MILLISECONDS);

        emitter.onCompletion(scheduler::shutdown);
        emitter.onTimeout(scheduler::shutdown);
        emitter.onError(e -> scheduler.shutdown());

        return emitter;
    }

    /**
     * SSE 根路径 — 列出可用端点并持续推送心跳。
     */
    @GetMapping(path = "", produces = MediaType.TEXT_EVENT_STREAM_VALUE)
    @Operation(summary = "SSE 入口，列出可用端点")
    public SseEmitter index() {
        SseEmitter emitter = new SseEmitter(300_000L); // 5 min timeout
        ScheduledExecutorService scheduler = Executors.newSingleThreadScheduledExecutor();

        try {
            emitter.send(SseEmitter.event()
                .name("welcome")
                .data("{\"welcome\":\"SSE 连接已建立\",\"endpoints\":[\"/sse/clock\",\"/sse/events\"]}")
                .build());
        } catch (Exception e) {
            emitter.completeWithError(e);
            scheduler.shutdown();
            return emitter;
        }

        scheduler.scheduleAtFixedRate(() -> {
            try {
                emitter.send(SseEmitter.event()
                    .name("heartbeat")
                    .data("{\"time\":\"" + LocalTime.now().format(TF) + "\"}")
                    .build());
            } catch (Exception e) {
                emitter.completeWithError(e);
                scheduler.shutdown();
            }
        }, 30, 30, TimeUnit.SECONDS);

        emitter.onCompletion(scheduler::shutdown);
        emitter.onTimeout(scheduler::shutdown);
        emitter.onError(e -> scheduler.shutdown());

        return emitter;
    }
}
