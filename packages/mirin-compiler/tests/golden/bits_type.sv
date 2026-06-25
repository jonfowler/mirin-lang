module f (
    input  logic clk,
    input  logic [7:0] raw,
    input  logic [7:0] key,
    output logic result
);
    logic [7:0] mask;
    assign mask = 8'hC8;
    logic hit;
    assign hit = (raw == key);
    logic m2;
    assign m2 = (mask == raw);
    assign result = (hit == m2);
endmodule
