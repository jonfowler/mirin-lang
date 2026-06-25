module f (
    input  logic clk,
    input  logic [7:0] x,
    output logic [7:0] result
);
    logic [7:0] m;
    assign m = (x + 8'h40);
    assign result = (m + 255);
endmodule

module g #(parameter int n) (
    input  logic [n-1:0] x,
    output logic [n-1:0] result
);
    assign result = (x + 1);
    initial begin
        assert (1 < (1 << n));
    end
endmodule
