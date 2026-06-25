module sum #(parameter int n) (
    input  logic clk,
    input  logic [7:0] v [0:n-1],
    output logic [7:0] result
);
    logic [7:0] acc;
    logic [7:0] x;
    always_comb begin
        acc = 0;
        for (int __i0 = 0; __i0 < n; __i0++) begin
            x = v[__i0];
            acc = (acc + x);
        end
    end
    assign result = acc;
endmodule
