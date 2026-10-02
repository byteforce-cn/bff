package com.example.fakesvc.controller;

import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.stereotype.Component;
import org.springframework.web.socket.CloseStatus;
import org.springframework.web.socket.TextMessage;
import org.springframework.web.socket.WebSocketSession;
import org.springframework.web.socket.handler.TextWebSocketHandler;

import java.time.LocalTime;
import java.time.format.DateTimeFormatter;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.TimeUnit;

/**
 * WebSocket Clock — 连接后每秒推送当前时间。
 */
@Component
public class WsClockHandler extends TextWebSocketHandler {

    private static final Logger log = LoggerFactory.getLogger(WsClockHandler.class);
    private static final DateTimeFormatter TF = DateTimeFormatter.ofPattern("HH:mm:ss");

    @Override
    public void afterConnectionEstablished(WebSocketSession session) {
        log.info("WS Clock connected: {}", session.getId());
        ScheduledExecutorService scheduler = Executors.newSingleThreadScheduledExecutor();
        ScheduledFuture<?> future = scheduler.scheduleAtFixedRate(() -> {
            try {
                if (session.isOpen()) {
                    String time = LocalTime.now().format(TF);
                    session.sendMessage(new TextMessage("{\"time\":\"" + time + "\"}"));
                } else {
                    scheduler.shutdown();
                }
            } catch (Exception e) {
                scheduler.shutdown();
            }
        }, 0, 1, TimeUnit.SECONDS);

        // 在 session 属性中保存 scheduler 以便断开时关闭
        session.getAttributes().put("clockScheduler", scheduler);
    }

    @Override
    public void afterConnectionClosed(WebSocketSession session, CloseStatus status) {
        log.info("WS Clock disconnected: {} status={}", session.getId(), status);
        ScheduledExecutorService scheduler =
            (ScheduledExecutorService) session.getAttributes().get("clockScheduler");
        if (scheduler != null) {
            scheduler.shutdownNow();
        }
    }
}
