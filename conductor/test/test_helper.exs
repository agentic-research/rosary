# Compile test support modules
Code.require_file("support/mock_rsry.ex", __DIR__)
Code.require_file("support/mock_sprites.ex", __DIR__)

ExUnit.start(exclude: [:integration])
